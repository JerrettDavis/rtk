//! Filters directory listings into a compact tree format.

use super::constants::NOISE_DIRS;
use crate::core::runner::{self, RunOptions};
use crate::core::tracking::TimedExecution;
use crate::core::utils::resolved_command;
use anyhow::{Context, Result};
use lazy_static::lazy_static;
use regex::Regex;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

lazy_static! {
    /// Matches the date+time portion in `ls -la` output, which serves as a
    /// stable anchor regardless of owner/group column width.
    /// E.g.: " Mar 31 16:18 " or " Dec 25  2024 "
    static ref LS_DATE_RE: Regex = Regex::new(
        r"\s+(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+\d{1,2}\s+(?:\d{4}|\d{2}:\d{2})\s+"
    )
    .unwrap();
}

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let request = normalize_ls_args(args);

    #[cfg(target_os = "windows")]
    if crate::core::utils::resolve_binary("ls").is_err() {
        return run_windows_ls(args, &request, verbose);
    }

    let mut cmd = resolved_command("ls");
    cmd.arg("-la");
    if request.recursive {
        cmd.arg("-R");
    }
    for flag in &request.passthrough_flags {
        cmd.arg(flag);
    }

    if request.paths.is_empty() {
        cmd.arg(".");
    } else {
        for p in &request.paths {
            cmd.arg(p);
        }
    }

    let target_display = if request.paths.is_empty() {
        ".".to_string()
    } else {
        request.paths.join(" ")
    };
    let show_all = request.show_all;

    runner::run_filtered(
        cmd,
        "ls",
        &format!("-la {}", target_display),
        |raw| {
            let (entries, summary) = compact_ls(raw, show_all);

            // Only show summary in interactive mode (not when piped)
            let is_tty = std::io::stdout().is_terminal();
            let filtered = if is_tty {
                format!("{}{}", entries, summary)
            } else {
                entries
            };

            if verbose > 0 {
                eprintln!(
                    "Chars: {} → {} ({}% reduction)",
                    raw.len(),
                    filtered.len(),
                    if !raw.is_empty() {
                        100 - (filtered.len() * 100 / raw.len())
                    } else {
                        0
                    }
                );
            }
            filtered
        },
        RunOptions::stdout_only()
            .early_exit_on_failure()
            .no_trailing_newline(),
    )
}

#[cfg_attr(not(test), allow(dead_code))]
fn windows_ls_unavailable_message() -> &'static str {
    "`rtk ls` can translate PowerShell and cmd flags on Windows. Supported native aliases include `ls -Force`, `dir /a`, and `dir /s`, all routed through RTK compression."
}

#[derive(Debug, Default, PartialEq)]
struct LsRequest {
    show_all: bool,
    recursive: bool,
    paths: Vec<String>,
    passthrough_flags: Vec<String>,
}

#[derive(Debug)]
struct LsEntry {
    name: String,
    is_dir: bool,
    size: u64,
}

fn normalize_ls_args(args: &[String]) -> LsRequest {
    let mut request = LsRequest::default();
    let mut parsing_options = true;

    for arg in args {
        if parsing_options && arg == "--" {
            parsing_options = false;
            continue;
        }

        if parsing_options && arg.starts_with("--") {
            match arg.as_str() {
                "--all" => request.show_all = true,
                "--recursive" => request.recursive = true,
                "--long" | "--human-readable" => {}
                _ => request.passthrough_flags.push(arg.clone()),
            }
            continue;
        }

        if parsing_options && arg.starts_with('/') && arg.len() > 1 {
            match arg[1..].to_ascii_lowercase().as_str() {
                "a" => request.show_all = true,
                "s" => request.recursive = true,
                _ => request.passthrough_flags.push(arg.clone()),
            }
            continue;
        }

        if parsing_options && arg.starts_with('-') && arg.len() > 1 {
            if arg.eq_ignore_ascii_case("-force") {
                request.show_all = true;
                continue;
            }
            if arg.eq_ignore_ascii_case("-recurse") {
                request.recursive = true;
                continue;
            }

            let mut extra = String::new();
            for ch in arg.chars().skip(1) {
                match ch {
                    'a' => request.show_all = true,
                    'R' => request.recursive = true,
                    'l' | 'h' | '1' => {}
                    _ => extra.push(ch),
                }
            }
            if !extra.is_empty() {
                request.passthrough_flags.push(format!("-{}", extra));
            }
            continue;
        }

        parsing_options = false;
        request.paths.push(arg.clone());
    }

    request
}

fn run_windows_ls(args: &[String], request: &LsRequest, verbose: u8) -> Result<i32> {
    let timer = TimedExecution::start();
    let output = render_windows_ls(request)?;

    if verbose > 0 {
        eprintln!("Running Windows-native ls fallback");
    }

    print!("{output}");

    let original_cmd = if args.is_empty() {
        "ls".to_string()
    } else {
        format!("ls {}", args.join(" "))
    };
    let rtk_cmd = if args.is_empty() {
        "rtk ls".to_string()
    } else {
        format!("rtk ls {}", args.join(" "))
    };
    timer.track(&original_cmd, &rtk_cmd, &output, &output);
    Ok(0)
}

fn render_windows_ls(request: &LsRequest) -> Result<String> {
    let mut entries = Vec::new();
    let targets: Vec<PathBuf> = if request.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        request.paths.iter().map(PathBuf::from).collect()
    };

    for target in &targets {
        let metadata = fs::metadata(target)
            .with_context(|| format!("failed to read metadata for {}", target.display()))?;
        if metadata.is_dir() {
            collect_dir_entries(target, request, targets.len() > 1, &mut entries)?;
        } else {
            let display_name = normalize_display_path(target);
            if !should_skip_entry(&display_name, false, &metadata, request.show_all) {
                entries.push(LsEntry {
                    name: display_name,
                    is_dir: false,
                    size: metadata.len(),
                });
            }
        }
    }

    let (listing, summary) = format_compact_entries(&entries);
    let is_tty = std::io::stdout().is_terminal();
    Ok(if is_tty {
        format!("{listing}{summary}")
    } else {
        listing
    })
}

fn collect_dir_entries(
    root: &Path,
    request: &LsRequest,
    multiple_targets: bool,
    entries: &mut Vec<LsEntry>,
) -> Result<()> {
    if request.recursive {
        for entry in WalkDir::new(root).min_depth(1) {
            let entry = entry.with_context(|| format!("failed to walk {}", root.display()))?;
            let metadata = entry
                .metadata()
                .with_context(|| format!("failed to read metadata for {}", entry.path().display()))?;
            let display_name = display_entry_name(root, entry.path(), true, multiple_targets);
            if should_skip_entry(&display_name, metadata.is_dir(), &metadata, request.show_all) {
                continue;
            }
            entries.push(LsEntry {
                name: display_name,
                is_dir: metadata.is_dir(),
                size: metadata.len(),
            });
        }
        return Ok(());
    }

    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry.with_context(|| format!("failed to read {}", root.display()))?;
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata for {}", entry.path().display()))?;
        let display_name = display_entry_name(root, &entry.path(), false, multiple_targets);
        if should_skip_entry(&display_name, metadata.is_dir(), &metadata, request.show_all) {
            continue;
        }
        entries.push(LsEntry {
            name: display_name,
            is_dir: metadata.is_dir(),
            size: metadata.len(),
        });
    }

    Ok(())
}

fn display_entry_name(root: &Path, path: &Path, recursive: bool, multiple_targets: bool) -> String {
    if recursive {
        let relative = path.strip_prefix(root).unwrap_or(path);
        if multiple_targets && root != Path::new(".") {
            let root_name = normalize_display_path(root);
            let rel_name = normalize_display_path(relative);
            if rel_name.is_empty() {
                root_name
            } else {
                format!("{root_name}/{rel_name}")
            }
        } else {
            normalize_display_path(relative)
        }
    } else if multiple_targets {
        normalize_display_path(path)
    } else {
        path.file_name()
            .map(|name| name.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| normalize_display_path(path))
    }
}

fn normalize_display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn should_skip_entry(name: &str, is_dir: bool, metadata: &fs::Metadata, show_all: bool) -> bool {
    if show_all {
        return false;
    }

    let file_name = Path::new(name)
        .file_name()
        .map(|value| value.to_string_lossy())
        .unwrap_or_default();

    (is_dir && NOISE_DIRS.iter().any(|noise| *noise == file_name))
        || is_hidden_entry(&file_name, metadata)
}

#[cfg(target_os = "windows")]
fn is_hidden_entry(file_name: &str, metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    file_name.starts_with('.') || (metadata.file_attributes() & 0x2) != 0
}

#[cfg(not(target_os = "windows"))]
fn is_hidden_entry(file_name: &str, _metadata: &fs::Metadata) -> bool {
    file_name.starts_with('.')
}

fn format_compact_entries(entries: &[LsEntry]) -> (String, String) {
    use std::collections::HashMap;

    if entries.is_empty() {
        return ("(empty)\n".to_string(), String::new());
    }

    let mut dirs: Vec<&LsEntry> = entries.iter().filter(|entry| entry.is_dir).collect();
    let mut files: Vec<&LsEntry> = entries.iter().filter(|entry| !entry.is_dir).collect();
    dirs.sort_by(|a, b| a.name.cmp(&b.name));
    files.sort_by(|a, b| a.name.cmp(&b.name));

    let mut by_ext: HashMap<String, usize> = HashMap::new();
    for file in &files {
        let ext = file
            .name
            .rsplit_once('.')
            .map(|(_, ext)| format!(".{ext}"))
            .unwrap_or_else(|| "no ext".to_string());
        *by_ext.entry(ext).or_insert(0) += 1;
    }

    let mut listing = String::new();
    for dir in &dirs {
        listing.push_str(&dir.name);
        listing.push_str("/\n");
    }
    for file in &files {
        listing.push_str(&file.name);
        listing.push_str("  ");
        listing.push_str(&human_size(file.size));
        listing.push('\n');
    }

    let mut summary = format!("\nSummary: {} files, {} dirs", files.len(), dirs.len());
    if !by_ext.is_empty() {
        let mut ext_counts: Vec<_> = by_ext.iter().collect();
        ext_counts.sort_by(|a, b| b.1.cmp(a.1));
        let ext_parts: Vec<String> = ext_counts
            .iter()
            .take(5)
            .map(|(ext, count)| format!("{} {}", count, ext))
            .collect();
        summary.push_str(" (");
        summary.push_str(&ext_parts.join(", "));
        if ext_counts.len() > 5 {
            summary.push_str(&format!(", +{} more", ext_counts.len() - 5));
        }
        summary.push(')');
    }
    summary.push('\n');

    (listing, summary)
}

/// Format bytes into human-readable size
fn human_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1}M", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else {
        format!("{}B", bytes)
    }
}

/// Parse a single `ls -la` line, returning `(file_type_char, size, name)`.
///
/// Uses the date field as a stable anchor — the date format in `ls -la` is
/// always three tokens (`Mon DD HH:MM` or `Mon DD  YYYY`), so we locate it
/// with a regex, then extract size (rightmost number before the date) and
/// filename (everything after the date). This handles owner/group names that
/// contain spaces, which break the old fixed-column approach.
fn parse_ls_line(line: &str) -> Option<(char, u64, String)> {
    let date_match = LS_DATE_RE.find(line)?;
    let name = line[date_match.end()..].to_string();

    let before_date = &line[..date_match.start()];
    let before_parts: Vec<&str> = before_date.split_whitespace().collect();
    if before_parts.len() < 4 {
        return None;
    }

    let perms = before_parts[0];
    let file_type = perms.chars().next()?;

    // Size is the rightmost parseable number before the date.
    // nlinks is also numeric but appears earlier; scanning from the end
    // guarantees we hit the size field first.
    let mut size: u64 = 0;
    for part in before_parts.iter().rev() {
        if let Ok(s) = part.parse::<u64>() {
            size = s;
            break;
        }
    }

    Some((file_type, size, name))
}

/// Parse ls -la output into compact format:
///   name/  (dirs)
///   name  size  (files)
/// Returns (entries, summary) so caller can suppress summary when piped.
fn compact_ls(raw: &str, show_all: bool) -> (String, String) {
    use std::collections::HashMap;

    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<(String, String)> = Vec::new(); // (name, size)
    let mut by_ext: HashMap<String, usize> = HashMap::new();

    for line in raw.lines() {
        if line.starts_with("total ") || line.is_empty() {
            continue;
        }

        let Some((file_type, size, name)) = parse_ls_line(line) else {
            continue;
        };

        // Skip . and ..
        if name == "." || name == ".." {
            continue;
        }

        // Filter noise dirs unless -a
        if !show_all && NOISE_DIRS.iter().any(|noise| name == *noise) {
            continue;
        }

        if file_type == 'd' {
            dirs.push(name);
        } else if file_type == '-' || file_type == 'l' {
            let ext = if let Some(pos) = name.rfind('.') {
                name[pos..].to_string()
            } else {
                "no ext".to_string()
            };
            *by_ext.entry(ext).or_insert(0) += 1;
            files.push((name, human_size(size)));
        }
    }

    if dirs.is_empty() && files.is_empty() {
        return ("(empty)\n".to_string(), String::new());
    }

    let mut entries = String::new();

    // Dirs first, compact
    for d in &dirs {
        entries.push_str(d);
        entries.push_str("/\n");
    }

    // Files with size
    for (name, size) in &files {
        entries.push_str(name);
        entries.push_str("  ");
        entries.push_str(size);
        entries.push('\n');
    }

    // Summary line (separate so caller can suppress when piped)
    let mut summary = format!("\nSummary: {} files, {} dirs", files.len(), dirs.len());
    if !by_ext.is_empty() {
        let mut ext_counts: Vec<_> = by_ext.iter().collect();
        ext_counts.sort_by(|a, b| b.1.cmp(a.1));
        let ext_parts: Vec<String> = ext_counts
            .iter()
            .take(5)
            .map(|(ext, count)| format!("{} {}", count, ext))
            .collect();
        summary.push_str(" (");
        summary.push_str(&ext_parts.join(", "));
        if ext_counts.len() > 5 {
            summary.push_str(&format!(", +{} more", ext_counts.len() - 5));
        }
        summary.push(')');
    }
    summary.push('\n');

    (entries, summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_basic() {
        let input = "total 48\n\
                     drwxr-xr-x  2 user  staff    64 Jan  1 12:00 .\n\
                     drwxr-xr-x  2 user  staff    64 Jan  1 12:00 ..\n\
                     drwxr-xr-x  2 user  staff    64 Jan  1 12:00 src\n\
                     -rw-r--r--  1 user  staff  1234 Jan  1 12:00 Cargo.toml\n\
                     -rw-r--r--  1 user  staff  5678 Jan  1 12:00 README.md\n";
        let (entries, _summary) = compact_ls(input, false);
        assert!(entries.contains("src/"));
        assert!(entries.contains("Cargo.toml"));
        assert!(entries.contains("README.md"));
        assert!(entries.contains("1.2K")); // 1234 bytes
        assert!(entries.contains("5.5K")); // 5678 bytes
        assert!(!entries.contains("drwx")); // no permissions
        assert!(!entries.contains("staff")); // no group
        assert!(!entries.contains("total")); // no total
        assert!(!entries.contains("\n.\n")); // no . entry
        assert!(!entries.contains("\n..\n")); // no .. entry
    }

    #[test]
    fn test_compact_filters_noise() {
        let input = "total 8\n\
                     drwxr-xr-x  2 user  staff  64 Jan  1 12:00 node_modules\n\
                     drwxr-xr-x  2 user  staff  64 Jan  1 12:00 .git\n\
                     drwxr-xr-x  2 user  staff  64 Jan  1 12:00 target\n\
                     drwxr-xr-x  2 user  staff  64 Jan  1 12:00 src\n\
                     -rw-r--r--  1 user  staff  100 Jan  1 12:00 main.rs\n";
        let (entries, _summary) = compact_ls(input, false);
        assert!(!entries.contains("node_modules"));
        assert!(!entries.contains(".git"));
        assert!(!entries.contains("target"));
        assert!(entries.contains("src/"));
        assert!(entries.contains("main.rs"));
    }

    #[test]
    fn test_compact_show_all() {
        let input = "total 8\n\
                     drwxr-xr-x  2 user  staff  64 Jan  1 12:00 .git\n\
                     drwxr-xr-x  2 user  staff  64 Jan  1 12:00 src\n";
        let (entries, _summary) = compact_ls(input, true);
        assert!(entries.contains(".git/"));
        assert!(entries.contains("src/"));
    }

    #[test]
    fn test_compact_empty() {
        let input = "total 0\n";
        let (entries, summary) = compact_ls(input, false);
        assert_eq!(entries, "(empty)\n");
        assert!(summary.is_empty());
    }

    #[test]
    fn test_compact_summary() {
        let input = "total 48\n\
                     drwxr-xr-x  2 user  staff    64 Jan  1 12:00 src\n\
                     -rw-r--r--  1 user  staff  1234 Jan  1 12:00 main.rs\n\
                     -rw-r--r--  1 user  staff  5678 Jan  1 12:00 lib.rs\n\
                     -rw-r--r--  1 user  staff   100 Jan  1 12:00 Cargo.toml\n";
        let (_entries, summary) = compact_ls(input, false);
        assert!(summary.contains("Summary: 3 files, 1 dirs"));
        assert!(summary.contains(".rs"));
        assert!(summary.contains(".toml"));
    }

    #[test]
    fn test_human_size() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(500), "500B");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1234), "1.2K");
        assert_eq!(human_size(1_048_576), "1.0M");
        assert_eq!(human_size(2_500_000), "2.4M");
    }

    #[test]
    fn test_compact_handles_filenames_with_spaces() {
        let input = "total 8\n\
                     -rw-r--r--  1 user  staff  1234 Jan  1 12:00 my file.txt\n";
        let (entries, _summary) = compact_ls(input, false);
        assert!(entries.contains("my file.txt"));
    }

    #[test]
    fn test_compact_symlinks() {
        let input = "total 8\n\
                     lrwxr-xr-x  1 user  staff  10 Jan  1 12:00 link -> target\n";
        let (entries, _summary) = compact_ls(input, false);
        assert!(entries.contains("link -> target"));
    }

    #[test]
    fn test_entries_no_summary() {
        // Entries should never contain the summary line
        let input = "total 48\n\
                     drwxr-xr-x  2 user  staff    64 Jan  1 12:00 src\n\
                     -rw-r--r--  1 user  staff  1234 Jan  1 12:00 main.rs\n";
        let (entries, summary) = compact_ls(input, false);
        assert!(
            !entries.contains("Summary:"),
            "entries must not contain summary"
        );
        assert!(
            summary.contains("Summary:"),
            "summary must contain the icon"
        );
    }

    #[test]
    fn test_pipe_line_count() {
        // Simulates: rtk ls | wc -l
        // Entries should have exactly 1 line per file/dir, no extra blank or summary
        let input = "total 48\n\
                     drwxr-xr-x  2 user  staff    64 Jan  1 12:00 src\n\
                     -rw-r--r--  1 user  staff  1234 Jan  1 12:00 main.rs\n\
                     -rw-r--r--  1 user  staff  5678 Jan  1 12:00 lib.rs\n";
        let (entries, _summary) = compact_ls(input, false);
        let line_count = entries.lines().count();
        assert_eq!(
            line_count, 3,
            "pipe should see exactly 3 lines (1 dir + 2 files), got {}",
            line_count
        );
    }

    // Regression test for #948: owner/group with spaces breaks fixed-column parsing
    #[test]
    fn test_compact_multiline_group() {
        let input = "total 8\n\
                     -rw-r--r--  1 fjeanne utilisa. du domaine    0 Mar 31 16:18 empty.txt\n\
                     -rw-r--r--  1 fjeanne utilisa. du domaine 1234 Mar 31 16:18 data.json\n";
        let (entries, _summary) = compact_ls(input, false);
        assert!(
            entries.contains("empty.txt"),
            "should contain 'empty.txt', got: {entries}"
        );
        assert!(
            entries.contains("data.json"),
            "should contain 'data.json', got: {entries}"
        );
        assert!(
            !entries.contains("16:18"),
            "time should not leak into filename, got: {entries}"
        );
        assert!(
            entries.contains("0B"),
            "empty.txt should show 0B, got: {entries}"
        );
        assert!(
            entries.contains("1.2K"),
            "data.json should show 1.2K (1234 bytes), got: {entries}"
        );
    }

    #[test]
    fn test_compact_year_format_date() {
        // Some systems show year instead of time for old files
        let input = "total 8\n\
                     -rw-r--r--  1 user staff  5678 Dec 25  2024 archive.tar\n";
        let (entries, _summary) = compact_ls(input, false);
        assert!(
            entries.contains("archive.tar"),
            "should contain filename, got: {entries}"
        );
        assert!(
            entries.contains("5.5K"),
            "should show 5.5K, got: {entries}"
        );
    }

    #[test]
    fn test_parse_ls_line_basic() {
        let (ft, size, name) = parse_ls_line(
            "-rw-r--r--  1 user staff 1234 Jan  1 12:00 file.txt",
        )
        .unwrap();
        assert_eq!(ft, '-');
        assert_eq!(size, 1234);
        assert_eq!(name, "file.txt");
    }

    #[test]
    fn test_parse_ls_line_multiline_group() {
        let (ft, size, name) = parse_ls_line(
            "-rw-r--r--  1 fjeanne utilisa. du domaine 0 Mar 31 16:18 empty.txt",
        )
        .unwrap();
        assert_eq!(ft, '-');
        assert_eq!(size, 0);
        assert_eq!(name, "empty.txt");
    }

    #[test]
    fn test_parse_ls_line_dir_with_space_in_group() {
        let (ft, size, name) = parse_ls_line(
            "drwxr-xr-x  2 fjeanne utilisa. du domaine 64 Mar 31 16:18 my dir",
        )
        .unwrap();
        assert_eq!(ft, 'd');
        assert_eq!(size, 64);
        assert_eq!(name, "my dir");
    }

    #[test]
    fn test_parse_ls_line_symlink() {
        let (ft, size, name) = parse_ls_line(
            "lrwxr-xr-x  1 user staff 10 Jan  1 12:00 link -> target",
        )
        .unwrap();
        assert_eq!(ft, 'l');
        assert_eq!(size, 10);
        assert_eq!(name, "link -> target");
    }

    #[test]
    fn test_parse_ls_line_returns_none_for_total() {
        assert!(parse_ls_line("total 48").is_none());
    }

    #[test]
    fn test_parse_ls_line_year_format() {
        let (ft, size, name) = parse_ls_line(
            "-rw-r--r--  1 user staff 5678 Dec 25  2024 old.tar.gz",
        )
        .unwrap();
        assert_eq!(ft, '-');
        assert_eq!(size, 5678);
        assert_eq!(name, "old.tar.gz");
    }

    #[test]
    fn test_windows_ls_unavailable_message_mentions_windows_alias_support() {
        let message = windows_ls_unavailable_message();
        assert!(message.contains("dir /a"));
        assert!(message.contains("rtk ls"));
    }

    #[test]
    fn test_normalize_ls_args_accepts_powershell_and_cmd_flags() {
        let args = vec![
            "-Force".to_string(),
            "/s".to_string(),
            "src".to_string(),
        ];
        let request = normalize_ls_args(&args);
        assert!(request.show_all);
        assert!(request.recursive);
        assert_eq!(request.paths, vec!["src"]);
    }
}
