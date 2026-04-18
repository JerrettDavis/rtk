/// Compact filter for `wc` — strips redundant paths and alignment padding.
///
/// Compression examples:
/// - `wc file.py`     → `30L 96W 978B`
/// - `wc -l file.py`  → `30`
/// - `wc -w file.py`  → `96`
/// - `wc -c file.py`  → `978`
/// - `wc -l *.py`     → table with common path prefix stripped
use crate::core::runner::{self, RunOptions};
use crate::core::tracking::TimedExecution;
use crate::core::utils::resolved_command;
use anyhow::{Context, Result};
use std::fs;

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    #[cfg(target_os = "windows")]
    if crate::core::utils::resolve_binary("wc").is_err() {
        return run_windows_wc(args, verbose);
    }

    let mut cmd = resolved_command("wc");
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: wc {}", args.join(" "));
    }

    let mode = detect_mode(args);
    runner::run_filtered(
        cmd,
        "wc",
        &args.join(" "),
        |stdout| filter_wc_output(stdout, &mode),
        RunOptions::stdout_only(),
    )
}

fn windows_wc_unavailable_message() -> &'static str {
    "`rtk wc` falls back to native Windows counting when no standalone `wc` executable is available. PowerShell and cmd-native rewrites still flow through RTK compression."
}

/// Which columns the user requested
#[derive(Debug, PartialEq)]
enum WcMode {
    /// Default: lines, words, bytes (3 columns)
    Full,
    /// Lines only (-l)
    Lines,
    /// Words only (-w)
    Words,
    /// Bytes only (-c)
    Bytes,
    /// Chars only (-m)
    Chars,
    /// Multiple flags combined — keep compact format
    Mixed,
}

#[derive(Debug, Default, Clone, Copy)]
struct WcStats {
    lines: usize,
    words: usize,
    bytes: usize,
    chars: usize,
}

fn detect_mode(args: &[String]) -> WcMode {
    let flags: Vec<&str> = args
        .iter()
        .filter(|a| a.starts_with('-'))
        .map(|s| s.as_str())
        .collect();

    if flags.is_empty() {
        return WcMode::Full;
    }

    // Collect all single-char flags (handles combined flags like -lw)
    let mut has_l = false;
    let mut has_w = false;
    let mut has_c = false;
    let mut has_m = false;
    let mut flag_count = 0;

    for flag in &flags {
        for ch in flag.chars().skip(1) {
            match ch {
                'l' => {
                    has_l = true;
                    flag_count += 1;
                }
                'w' => {
                    has_w = true;
                    flag_count += 1;
                }
                'c' => {
                    has_c = true;
                    flag_count += 1;
                }
                'm' => {
                    has_m = true;
                    flag_count += 1;
                }
                _ => {}
            }
        }
    }

    if flag_count == 0 {
        return WcMode::Full;
    }
    if flag_count > 1 {
        return WcMode::Mixed;
    }

    if has_l {
        WcMode::Lines
    } else if has_w {
        WcMode::Words
    } else if has_c {
        WcMode::Bytes
    } else if has_m {
        WcMode::Chars
    } else {
        WcMode::Full
    }
}

fn run_windows_wc(args: &[String], verbose: u8) -> Result<i32> {
    let timer = TimedExecution::start();
    let output = render_windows_wc(args)?;

    if verbose > 0 {
        eprintln!("Running Windows-native wc fallback");
    }

    print!("{output}");

    let original_cmd = if args.is_empty() {
        "wc".to_string()
    } else {
        format!("wc {}", args.join(" "))
    };
    let rtk_cmd = if args.is_empty() {
        "rtk wc".to_string()
    } else {
        format!("rtk wc {}", args.join(" "))
    };
    timer.track(&original_cmd, &rtk_cmd, &output, &output);
    Ok(0)
}

fn render_windows_wc(args: &[String]) -> Result<String> {
    let mode = detect_mode(args);
    let paths = extract_wc_paths(args);
    if paths.is_empty() {
        anyhow::bail!("{}", windows_wc_unavailable_message());
    }

    let mut rows = Vec::new();
    let mut total = WcStats::default();

    for path in &paths {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path))?;
        let stats = count_wc_stats(&bytes);
        total.lines += stats.lines;
        total.words += stats.words;
        total.bytes += stats.bytes;
        total.chars += stats.chars;
        rows.push((stats, path.clone()));
    }

    if rows.len() == 1 {
        return Ok(format_single_wc_stats(rows[0].0, &mode));
    }

    let mut rendered = rows
        .into_iter()
        .map(|(stats, path)| format!("{} {}", format_single_wc_stats(stats, &mode), path))
        .collect::<Vec<_>>();
    rendered.push(format!("Σ {}", format_single_wc_stats(total, &mode)));
    Ok(rendered.join("\n"))
}

fn extract_wc_paths(args: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut parsing_options = true;

    for arg in args {
        if parsing_options && arg == "--" {
            parsing_options = false;
            continue;
        }

        if parsing_options && arg.starts_with('-') {
            continue;
        }

        parsing_options = false;
        paths.push(arg.clone());
    }

    paths
}

fn count_wc_stats(bytes: &[u8]) -> WcStats {
    let text = String::from_utf8_lossy(bytes);
    WcStats {
        lines: bytes.iter().filter(|byte| **byte == b'\n').count(),
        words: text.split_whitespace().count(),
        bytes: bytes.len(),
        chars: text.chars().count(),
    }
}

fn format_single_wc_stats(stats: WcStats, mode: &WcMode) -> String {
    match mode {
        WcMode::Lines => stats.lines.to_string(),
        WcMode::Words => stats.words.to_string(),
        WcMode::Bytes => stats.bytes.to_string(),
        WcMode::Chars => stats.chars.to_string(),
        WcMode::Full => format!("{}L {}W {}B", stats.lines, stats.words, stats.bytes),
        WcMode::Mixed => format!(
            "{} {} {} {}",
            stats.lines, stats.words, stats.bytes, stats.chars
        ),
    }
}

fn filter_wc_output(raw: &str, mode: &WcMode) -> String {
    let lines: Vec<&str> = raw.trim().lines().collect();

    if lines.is_empty() {
        return String::new();
    }

    // Single file (one output line, no "total")
    if lines.len() == 1 {
        return format_single_line(lines[0], mode);
    }

    // Multiple files — compact table
    format_multi_line(&lines, mode)
}

/// Format a single wc output line (one file or stdin)
fn format_single_line(line: &str, mode: &WcMode) -> String {
    let parts: Vec<&str> = line.split_whitespace().collect();

    match mode {
        WcMode::Lines | WcMode::Words | WcMode::Bytes | WcMode::Chars => {
            // First number is the only requested column
            parts.first().map(|s| s.to_string()).unwrap_or_default()
        }
        WcMode::Full => {
            if parts.len() >= 3 {
                format!("{}L {}W {}B", parts[0], parts[1], parts[2])
            } else {
                line.trim().to_string()
            }
        }
        WcMode::Mixed => {
            // Strip file path, keep numbers only
            if parts.len() >= 2 {
                let last_is_path = parts.last().is_some_and(|p| p.parse::<u64>().is_err());
                if last_is_path {
                    parts[..parts.len() - 1].join(" ")
                } else {
                    parts.join(" ")
                }
            } else {
                line.trim().to_string()
            }
        }
    }
}

/// Format multiple files as a compact table
fn format_multi_line(lines: &[&str], mode: &WcMode) -> String {
    let mut result = Vec::new();

    // Find common directory prefix to shorten paths
    let paths: Vec<&str> = lines
        .iter()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            parts.last().copied()
        })
        .filter(|p| *p != "total")
        .collect();

    let common_prefix = find_common_prefix(&paths);

    for line in lines {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        let is_total = parts.last().is_some_and(|p| *p == "total");

        match mode {
            WcMode::Lines | WcMode::Words | WcMode::Bytes | WcMode::Chars => {
                if is_total {
                    result.push(format!("Σ {}", parts.first().unwrap_or(&"0")));
                } else {
                    let name = strip_prefix(parts.last().unwrap_or(&""), &common_prefix);
                    result.push(format!("{} {}", parts.first().unwrap_or(&"0"), name));
                }
            }
            WcMode::Full => {
                if is_total {
                    result.push(format!(
                        "Σ {}L {}W {}B",
                        parts.first().unwrap_or(&"0"),
                        parts.get(1).unwrap_or(&"0"),
                        parts.get(2).unwrap_or(&"0"),
                    ));
                } else if parts.len() >= 4 {
                    let name = strip_prefix(parts[3], &common_prefix);
                    result.push(format!(
                        "{}L {}W {}B {}",
                        parts[0], parts[1], parts[2], name
                    ));
                } else {
                    result.push(line.trim().to_string());
                }
            }
            WcMode::Mixed => {
                if is_total {
                    let nums: Vec<&str> = parts[..parts.len() - 1].to_vec();
                    result.push(format!("Σ {}", nums.join(" ")));
                } else if parts.len() >= 2 {
                    let last_is_path = parts.last().is_some_and(|p| p.parse::<u64>().is_err());
                    if last_is_path {
                        let name = strip_prefix(parts.last().unwrap_or(&""), &common_prefix);
                        let nums: Vec<&str> = parts[..parts.len() - 1].to_vec();
                        result.push(format!("{} {}", nums.join(" "), name));
                    } else {
                        result.push(parts.join(" "));
                    }
                } else {
                    result.push(line.trim().to_string());
                }
            }
        }
    }

    result.join("\n")
}

/// Find common directory prefix among paths
fn find_common_prefix(paths: &[&str]) -> String {
    if paths.len() <= 1 {
        return String::new();
    }

    let first = paths[0];
    let prefix = if let Some(pos) = first.rfind('/') {
        &first[..=pos]
    } else {
        return String::new();
    };

    if paths.iter().all(|p| p.starts_with(prefix)) {
        return prefix.to_string();
    }

    // Try shorter prefixes by removing right-most segments
    let mut candidate = prefix.to_string();
    while !candidate.is_empty() {
        if paths.iter().all(|p| p.starts_with(&candidate)) {
            return candidate;
        }
        if let Some(pos) = candidate[..candidate.len() - 1].rfind('/') {
            candidate.truncate(pos + 1);
        } else {
            return String::new();
        }
    }
    String::new()
}

/// Strip common prefix from a path
fn strip_prefix<'a>(path: &'a str, prefix: &str) -> &'a str {
    if prefix.is_empty() {
        return path;
    }
    path.strip_prefix(prefix).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_file_full() {
        let raw = "      30      96     978 scripts/find_duplicate_attrs.py\n";
        let result = filter_wc_output(raw, &WcMode::Full);
        assert_eq!(result, "30L 96W 978B");
    }

    #[test]
    fn test_single_file_lines_only() {
        let raw = "      30 scripts/find_duplicate_attrs.py\n";
        let result = filter_wc_output(raw, &WcMode::Lines);
        assert_eq!(result, "30");
    }

    #[test]
    fn test_single_file_words_only() {
        let raw = "      96 scripts/find_duplicate_attrs.py\n";
        let result = filter_wc_output(raw, &WcMode::Words);
        assert_eq!(result, "96");
    }

    #[test]
    fn test_stdin_full() {
        let raw = "      30      96     978\n";
        let result = filter_wc_output(raw, &WcMode::Full);
        assert_eq!(result, "30L 96W 978B");
    }

    #[test]
    fn test_stdin_lines() {
        let raw = "      30\n";
        let result = filter_wc_output(raw, &WcMode::Lines);
        assert_eq!(result, "30");
    }

    #[test]
    fn test_multi_file_lines() {
        let raw = "      30 src/main.rs\n      50 src/lib.rs\n      80 total\n";
        let result = filter_wc_output(raw, &WcMode::Lines);
        assert_eq!(result, "30 main.rs\n50 lib.rs\nΣ 80");
    }

    #[test]
    fn test_multi_file_full() {
        let raw = "      30      96     978 src/main.rs\n      50     120    1500 src/lib.rs\n      80     216    2478 total\n";
        let result = filter_wc_output(raw, &WcMode::Full);
        assert_eq!(
            result,
            "30L 96W 978B main.rs\n50L 120W 1500B lib.rs\nΣ 80L 216W 2478B"
        );
    }

    #[test]
    fn test_detect_mode_full() {
        let args: Vec<String> = vec!["file.py".into()];
        assert_eq!(detect_mode(&args), WcMode::Full);
    }

    #[test]
    fn test_detect_mode_lines() {
        let args: Vec<String> = vec!["-l".into(), "file.py".into()];
        assert_eq!(detect_mode(&args), WcMode::Lines);
    }

    #[test]
    fn test_detect_mode_mixed() {
        let args: Vec<String> = vec!["-lw".into(), "file.py".into()];
        assert_eq!(detect_mode(&args), WcMode::Mixed);
    }

    #[test]
    fn test_detect_mode_separate_flags() {
        let args: Vec<String> = vec!["-l".into(), "-w".into(), "file.py".into()];
        assert_eq!(detect_mode(&args), WcMode::Mixed);
    }

    #[test]
    fn test_common_prefix() {
        let paths = vec!["src/main.rs", "src/lib.rs", "src/utils.rs"];
        assert_eq!(find_common_prefix(&paths), "src/");
    }

    #[test]
    fn test_no_common_prefix() {
        let paths = vec!["main.rs", "lib.rs"];
        assert_eq!(find_common_prefix(&paths), "");
    }

    #[test]
    fn test_deep_common_prefix() {
        let paths = vec!["src/cmd/wc.rs", "src/cmd/ls.rs"];
        assert_eq!(find_common_prefix(&paths), "src/cmd/");
    }

    #[test]
    fn test_empty() {
        let raw = "";
        let result = filter_wc_output(raw, &WcMode::Full);
        assert_eq!(result, "");
    }

    #[test]
    fn test_windows_wc_unavailable_message_mentions_native_fallback() {
        let message = windows_wc_unavailable_message();
        assert!(message.contains("native Windows counting"));
        assert!(message.contains("RTK compression"));
    }

    #[test]
    fn test_windows_wc_fallback_counts_lines() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), "one\ntwo\nthree\n").unwrap();
        let args = vec!["-l".to_string(), temp.path().display().to_string()];
        let result = render_windows_wc(&args).unwrap();
        assert_eq!(result, "3");
    }
}
