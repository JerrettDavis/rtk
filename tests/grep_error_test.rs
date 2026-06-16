//! Integration test: rtk grep must NOT claim "0 matches" on error exit codes.
//!
//! grep/rg convention: exit 1 = no match (normal), exit >= 2 = real error.
//! rtk must surface the error instead of printing a false-negative "0 matches".

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn grep_error_exit_code_no_false_negative() {
    let dir = std::env::temp_dir().join(format!("rtk-test-grep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");

    // Test file with known content so "0 matches" is provably wrong
    let test_file = dir.join("test.txt");
    std::fs::write(&test_file, "hello world\n").expect("write test file");

    // Fake rg that exits with code 2 (error), empty output — simulates
    // a broken/missing rg binary or a ripgrep panic.
    let fake_rg = dir.join("rg");
    std::fs::write(&fake_rg, "#!/bin/sh\nexit 2\n").expect("write fake rg");
    std::fs::set_permissions(&fake_rg, std::fs::Permissions::from_mode(0o755))
        .expect("chmod fake rg");

    // Reference the rtk binary built by cargo for integration tests
    let rtk = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(if cfg!(debug_assertions) {
            "target/debug/rtk"
        } else {
            "target/release/rtk"
        });

    let path_env = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new(&rtk)
        .args(["grep", "hello"])
        .arg(test_file.to_str().unwrap())
        .env("PATH", &path_env)
        .env("RTK_HOOK_OFF", "1") // no hook inference
        .output()
        .expect("run rtk");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stdout.contains("0 matches"),
        "ERROR exit MUST NOT claim '0 matches' (false negative).\nstdout: {}\nstderr: {}",
        stdout,
        stderr,
    );

    // Error exit code should be non-zero (propagated from rg's exit 2)
    assert_ne!(
        output.status.code().unwrap_or(0),
        0,
        "rtk should exit non-zero on grep error.\nstdout: {}\nstderr: {}",
        stdout,
        stderr,
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&dir);
}
