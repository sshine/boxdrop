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
fn ssh_transport_uses_the_same_protocol() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let local_root = dir.path().join("local");
    let remote_root = dir.path().join("remote");
    let local_state = dir.path().join("local-state");
    let remote_state = dir.path().join("remote-state");
    fs::create_dir_all(&local_root).unwrap();
    fs::create_dir_all(&remote_root).unwrap();

    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let ssh = bin.join("ssh");
    fs::write(
        &ssh,
        "#!/bin/sh\nfor last; do :; done\nexec sh -c \"$last\"\n",
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();

    let boxdrop_bin = env!("CARGO_BIN_EXE_boxdrop");
    assert!(
        Command::new(boxdrop_bin)
            .args([
                "--state",
                local_state.to_string_lossy().as_ref(),
                "--root",
                local_root.to_string_lossy().as_ref(),
                "init",
                local_root.to_string_lossy().as_ref(),
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new(boxdrop_bin)
            .args([
                "--state",
                remote_state.to_string_lossy().as_ref(),
                "--root",
                remote_root.to_string_lossy().as_ref(),
                "init",
                remote_root.to_string_lossy().as_ref(),
            ])
            .status()
            .unwrap()
            .success()
    );
    fs::write(local_root.join("over-ssh"), b"ssh payload").unwrap();

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = Command::new(boxdrop_bin)
        .env("PATH", path)
        .args([
            "--state",
            local_state.to_string_lossy().as_ref(),
            "--root",
            local_root.to_string_lossy().as_ref(),
            "sync",
            "--ssh",
            "fake-host",
            "--remote-root",
            remote_root.to_string_lossy().as_ref(),
            "--remote-state",
            remote_state.to_string_lossy().as_ref(),
            "--remote-boxdrop",
            boxdrop_bin,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(remote_root.join("over-ssh")).unwrap(),
        b"ssh payload"
    );
}

#[test]
fn restore_and_diff_are_reachable_from_the_cli() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    let state = dir.path().join("state");
    fs::create_dir_all(&root).unwrap();
    let root_arg = root.to_string_lossy().into_owned();
    assert!(
        boxdrop(&state, &["--root", &root_arg, "init", &root_arg])
            .status
            .success()
    );
    fs::write(root.join("f"), b"one").unwrap();
    assert!(
        boxdrop(
            &state,
            &["--root", &root_arg, "snapshot", "--message", "one"]
        )
        .status
        .success()
    );
    let log = boxdrop(&state, &["--root", &root_arg, "log", "--json"]);
    let snapshots: Vec<serde_json::Value> = serde_json::from_slice(&log.stdout).unwrap();
    let first = snapshots
        .iter()
        .find(|snapshot| snapshot["message"] == "one")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    fs::write(root.join("f"), b"two").unwrap();
    assert!(
        boxdrop(
            &state,
            &["--root", &root_arg, "snapshot", "--message", "two"]
        )
        .status
        .success()
    );
    let log = boxdrop(&state, &["--root", &root_arg, "log", "--json"]);
    let snapshots: Vec<serde_json::Value> = serde_json::from_slice(&log.stdout).unwrap();
    let second = snapshots
        .iter()
        .find(|snapshot| snapshot["message"] == "two")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let diff = boxdrop(
        &state,
        &["--root", &root_arg, "diff", &first, &second, "--json"],
    );
    assert!(diff.status.success());
    let diff: Vec<serde_json::Value> = serde_json::from_slice(&diff.stdout).unwrap();
    assert!(diff.iter().any(|entry| entry["path"] == "f"));

    let restore_to = dir.path().join("restored");
    let restored = boxdrop(
        &state,
        &[
            "--root",
            &root_arg,
            "restore",
            &first,
            "--to",
            restore_to.to_string_lossy().as_ref(),
        ],
    );
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    assert_eq!(fs::read(restore_to.join("f")).unwrap(), b"one");
}

#[test]
fn peers_are_stored_per_root() {
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
    let root_arg = root.to_string_lossy().into_owned();
    assert!(
        boxdrop(
            &state,
            &["--root", &root_arg, "peer", "add", "laptop", "laptop:7777"]
        )
        .status
        .success()
    );
    let output = boxdrop(&state, &["--root", &root_arg, "peer", "list", "--json"]);
    assert!(output.status.success());
    let peers: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["name"], "laptop");
    assert!(
        boxdrop(&state, &["--root", &root_arg, "peer", "remove", "laptop"])
            .status
            .success()
    );
    let output = boxdrop(&state, &["--root", &root_arg, "peer", "list", "--json"]);
    let peers: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(peers.is_empty());
}

#[test]
fn native_status_reports_the_filesystem_capabilities() {
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
    let output = boxdrop(
        &state,
        &[
            "--root",
            root.to_string_lossy().as_ref(),
            "native",
            "status",
            "--json",
        ],
    );
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(matches!(
        value["kind"].as_str().unwrap(),
        "portable" | "reflink" | "zfs" | "btrfs"
    ));
    assert!(value["reflink"].is_boolean());
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
