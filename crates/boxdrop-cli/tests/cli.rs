#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

use tempfile::TempDir;

fn boxdrop(state: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_boxdrop"))
        .arg("--state")
        .arg(state)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn init_sync_status_and_log_work_without_a_daemon() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    let state = dir.path().join("state");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.txt"), b"alpha").unwrap();

    let output = boxdrop(
        &state,
        &[
            "--root",
            root.to_string_lossy().as_ref(),
            "init",
            root.to_string_lossy().as_ref(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::write(root.join("b.txt"), b"beta").unwrap();
    let output = boxdrop(
        &state,
        &[
            "--root",
            root.to_string_lossy().as_ref(),
            "sync",
            "--one-shot",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = boxdrop(&state, &["status", "--json"]);
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["files"], 2);
    assert!(status["snapshots"].as_u64().unwrap() >= 1);

    let output = boxdrop(&state, &["log"]);
    assert!(output.status.success());
    assert!(!output.stdout.is_empty());
}

#[test]
fn conflicts_lists_conflict_copies() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    let state = dir.path().join("state");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("f.boxdrop-conflict-host-1"), b"loser").unwrap();
    fs::write(root.join("normal"), b"winner").unwrap();
    assert!(
        boxdrop(
            &state,
            &[
                "--root",
                root.to_string_lossy().as_ref(),
                "init",
                root.to_string_lossy().as_ref()
            ]
        )
        .status
        .success()
    );
    let output = boxdrop(&state, &["conflicts"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("f.boxdrop-conflict-host-1"));
    assert!(!text.contains("normal"));
}

#[test]
fn pause_and_resume_persist_without_a_daemon() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    let state = dir.path().join("state");
    fs::create_dir_all(&root).unwrap();

    assert!(
        boxdrop(
            &state,
            &[
                "--root",
                root.to_string_lossy().as_ref(),
                "init",
                root.to_string_lossy().as_ref()
            ]
        )
        .status
        .success()
    );
    assert!(boxdrop(&state, &["pause"]).status.success());
    let output = boxdrop(&state, &["status", "--json"]);
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["paused"], true);
    assert!(boxdrop(&state, &["resume"]).status.success());
}
