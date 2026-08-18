use std::path::PathBuf;
use std::process::Command;

fn main() {
    tauri_build::build();
    check_panel_script();
}

/// Syntax-check the injected in-page script so a broken shell-panel.js fails
/// the build instead of silently producing a webview without its chrome.
fn check_panel_script() {
    let panel = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/shell-panel.js");
    println!("cargo:rerun-if-changed={}", panel.display());
    match Command::new("node").arg("--check").arg(&panel).status() {
        Ok(status) if status.success() => {}
        Ok(status) => panic!(
            "{} failed `node --check` (exit {status})",
            panel.display()
        ),
        // node is a documented dev prerequisite; absent node only skips the
        // check, it never breaks the build.
        Err(e) => println!("cargo:warning=node unavailable, skipping panel.js syntax check: {e}"),
    }
}
