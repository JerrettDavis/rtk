//! Detects whether RTK hooks are installed and warns if they are outdated.

use super::constants::{
    CLAUDE_DIR, CLAUDE_HOOK_COMMAND, CODEX_DIR, CURSOR_DIR, GEMINI_DIR, GEMINI_HOOK_FILE,
    HOOKS_SUBDIR, OPENCODE_PLUGIN_PATH, PRE_TOOL_USE_KEY, REWRITE_HOOK_FILE, REWRITE_HOOK_PS1_FILE,
    SETTINGS_JSON,
};
use crate::core::constants::RTK_DATA_DIR;
use std::path::PathBuf;

const CURRENT_HOOK_VERSION: u8 = 3;
const WARN_INTERVAL_SECS: u64 = 24 * 3600;

/// Hook status for diagnostics and `rtk gain`.
#[derive(Debug, PartialEq, Clone)]
pub enum HookStatus {
    /// Hook is installed and up to date.
    Ok,
    /// Hook exists but is outdated or unreadable.
    Outdated,
    /// No hook file found (but Claude Code is installed).
    Missing,
}

/// Return the current hook status without printing anything.
/// Returns `Ok` if no Claude Code is detected (not applicable).
pub fn status() -> HookStatus {
    // Don't warn users who don't have Claude Code installed
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return HookStatus::Ok,
    };
    let claude_dir = home.join(CLAUDE_DIR);
    if !claude_dir.exists() {
        return HookStatus::Ok;
    }

    // Check for new binary command in settings.json first
    if binary_hook_registered(&claude_dir) {
        // If old script file still exists alongside new command, report Outdated
        // (migration not complete — user should run `rtk init -g` to clean up)
        let old_hook = claude_dir.join(HOOKS_SUBDIR).join(REWRITE_HOOK_FILE);
        if old_hook.exists() {
            return HookStatus::Outdated;
        }
        return HookStatus::Ok;
    }

    // Fall back to legacy script file check
    let Some(hook_path) = hook_installed_path() else {
        return HookStatus::Missing;
    };
    let Ok(content) = std::fs::read_to_string(&hook_path) else {
        return HookStatus::Outdated; // exists but unreadable — treat as needs-update
    };
    if parse_hook_version(&content) >= CURRENT_HOOK_VERSION {
        HookStatus::Ok
    } else {
        HookStatus::Outdated
    }
}

/// Check if the native binary command is registered in settings.json
fn binary_hook_registered(claude_dir: &std::path::Path) -> bool {
    let settings_path = claude_dir.join(SETTINGS_JSON);
    let content = match std::fs::read_to_string(&settings_path) {
        Ok(c) if !c.trim().is_empty() => c,
        _ => return false,
    };
    let root: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let pre_tool_use = match root
        .get("hooks")
        .and_then(|h| h.get(PRE_TOOL_USE_KEY))
        .and_then(|p| p.as_array())
    {
        Some(arr) => arr,
        None => return false,
    };
    pre_tool_use
        .iter()
        .filter_map(|entry| entry.get("hooks")?.as_array())
        .flatten()
        .filter_map(|hook| hook.get("command")?.as_str())
        .any(|cmd| cmd == CLAUDE_HOOK_COMMAND)
}

/// Check if the installed hook is missing or outdated, warn once per day.
pub fn maybe_warn() {
    // Don't block startup — fail silently on any error
    let _ = check_and_warn();
}

/// Single source of truth: delegates to `status()` then rate-limits the warning.
fn check_and_warn() -> Option<()> {
    let warning = match status() {
        HookStatus::Ok => return Some(()),
        HookStatus::Missing => {
            "[rtk] /!\\ No hook installed — run `rtk init -g` for automatic token savings"
        }
        HookStatus::Outdated => "[rtk] /!\\ Hook outdated — run `rtk init -g` to update",
    };

    // Rate limit: warn once per day
    let marker = warn_marker_path()?;
    if let Ok(meta) = std::fs::metadata(&marker) {
        if let Ok(modified) = meta.modified() {
            if modified.elapsed().map(|e| e.as_secs()).unwrap_or(u64::MAX) < WARN_INTERVAL_SECS {
                return Some(());
            }
        }
    }

    eprintln!("{}", warning);

    // Touch marker after warning is printed
    let _ = std::fs::create_dir_all(marker.parent()?);
    let _ = std::fs::write(&marker, b"");

    Some(())
}

pub fn parse_hook_version(content: &str) -> u8 {
    // Version tag must be in the first 5 lines (shebang + header convention)
    for line in content.lines().take(5) {
        if let Some(rest) = line.strip_prefix("# rtk-hook-version:") {
            if let Ok(v) = rest.trim().parse::<u8>() {
                return v;
            }
        }
    }
    0 // No version tag = version 0 (outdated)
}

#[cfg(test)]
fn other_integration_installed(home: &std::path::Path) -> bool {
    let paths = [
        home.join(OPENCODE_PLUGIN_PATH),
        home.join(CURSOR_DIR)
            .join(HOOKS_SUBDIR)
            .join(REWRITE_HOOK_FILE),
        home.join(CLAUDE_DIR)
            .join(HOOKS_SUBDIR)
            .join(REWRITE_HOOK_PS1_FILE),
        home.join(CODEX_DIR).join("AGENTS.md"),
        home.join(GEMINI_DIR)
            .join(HOOKS_SUBDIR)
            .join(GEMINI_HOOK_FILE),
    ];
    paths.iter().any(|p| p.exists())
}

fn hook_installed_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    // Check bash hook first, then PowerShell hook
    let path = home
        .join(CLAUDE_DIR)
        .join(HOOKS_SUBDIR)
        .join(REWRITE_HOOK_FILE);
    if path.exists() {
        return Some(path);
    }
    let ps1_path = home
        .join(CLAUDE_DIR)
        .join(HOOKS_SUBDIR)
        .join(REWRITE_HOOK_PS1_FILE);
    if ps1_path.exists() {
        Some(ps1_path)
    } else {
        None
    }
}

fn warn_marker_path() -> Option<PathBuf> {
    let data_dir = dirs::data_local_dir()?.join(RTK_DATA_DIR);
    Some(data_dir.join(".hook_warn_last"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hook_version_present() {
        let content = "#!/usr/bin/env bash\n# rtk-hook-version: 2\n# some comment\n";
        assert_eq!(parse_hook_version(content), 2);
    }

    #[test]
    fn test_parse_hook_version_missing() {
        let content = "#!/usr/bin/env bash\n# old hook without version\n";
        assert_eq!(parse_hook_version(content), 0);
    }

    #[test]
    fn test_parse_hook_version_future() {
        let content = "#!/usr/bin/env bash\n# rtk-hook-version: 5\n";
        assert_eq!(parse_hook_version(content), 5);
    }

    #[test]
    fn test_parse_hook_version_no_tag() {
        assert_eq!(parse_hook_version("no version here"), 0);
        assert_eq!(parse_hook_version(""), 0);
    }

    #[test]
    fn test_hook_status_enum() {
        assert_ne!(HookStatus::Ok, HookStatus::Missing);
        assert_ne!(HookStatus::Outdated, HookStatus::Missing);
        assert_eq!(HookStatus::Ok, HookStatus::Ok);
        // Clone works
        let s = HookStatus::Missing;
        assert_eq!(s.clone(), HookStatus::Missing);
    }

    #[test]
    fn test_other_integration_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!other_integration_installed(tmp.path()));
    }

    #[test]
    fn test_other_integration_opencode() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(OPENCODE_PLUGIN_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"plugin").unwrap();
        assert!(other_integration_installed(tmp.path()));
    }

    #[test]
    fn test_other_integration_cursor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp
            .path()
            .join(CURSOR_DIR)
            .join(HOOKS_SUBDIR)
            .join(REWRITE_HOOK_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"hook").unwrap();
        assert!(other_integration_installed(tmp.path()));
    }

    #[test]
    fn test_other_integration_claude_powershell() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp
            .path()
            .join(CLAUDE_DIR)
            .join(HOOKS_SUBDIR)
            .join(REWRITE_HOOK_PS1_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"# rtk-hook-version: 3").unwrap();
        assert!(other_integration_installed(tmp.path()));
    }

    #[test]
    fn test_other_integration_codex() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(CODEX_DIR).join("AGENTS.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"agents").unwrap();
        assert!(other_integration_installed(tmp.path()));
    }

    #[test]
    fn test_other_integration_gemini() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp
            .path()
            .join(GEMINI_DIR)
            .join(HOOKS_SUBDIR)
            .join(GEMINI_HOOK_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"hook").unwrap();
        assert!(other_integration_installed(tmp.path()));
    }

    #[test]
    fn test_other_integration_empty_dirs_not_enough() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(CURSOR_DIR).join(HOOKS_SUBDIR)).unwrap();
        std::fs::create_dir_all(tmp.path().join(CODEX_DIR)).unwrap();
        std::fs::create_dir_all(tmp.path().join(GEMINI_DIR)).unwrap();
        assert!(!other_integration_installed(tmp.path()));
    }

    #[test]
    fn test_parse_hook_version_powershell_comment() {
        let content = "#!/usr/bin/env pwsh\n# rtk-hook-version: 3\n";
        assert_eq!(parse_hook_version(content), 3);
    }

    #[test]
    fn test_status_returns_valid_variant() {
        // Skip on machines without Claude Code
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => return,
        };
        let s = status();
        let has_claude_hook = home
            .join(".claude")
            .join("hooks")
            .join("rtk-rewrite.sh")
            .exists()
            || home
                .join(".claude")
                .join("hooks")
                .join("rtk-rewrite.ps1")
                .exists();
        let has_claude_dir = home.join(".claude").exists();
        let has_other = other_integration_installed(&home);

        match (has_claude_hook, has_claude_dir, has_other) {
            (true, _, _) => assert!(
                s == HookStatus::Ok || s == HookStatus::Outdated,
                "Expected Ok or Outdated when Claude hook exists, got {:?}",
                s
            ),
            (false, true, _) => assert_eq!(
                s,
                HookStatus::Missing,
                "Expected Missing when .claude/ exists but hook absent, got {:?}",
                s
            ),
            (false, false, _) => assert_eq!(s, HookStatus::Ok),
        }
    }
}
