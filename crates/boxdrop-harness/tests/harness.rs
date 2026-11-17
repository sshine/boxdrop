#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

#[test]
fn sim_converges() {
    let status = Command::new(env!("CARGO_BIN_EXE_boxdrop-harness"))
        .args(["sim", "--seeds", "2", "--steps", "20"])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn two_host_run_converges() {
    let status = Command::new(env!("CARGO_BIN_EXE_boxdrop-harness"))
        .args(["run", "--duration-secs", "2", "--rate", "20", "--no-userns"])
        .status()
        .unwrap();
    assert!(status.success());
}
