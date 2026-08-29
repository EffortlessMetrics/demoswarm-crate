use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;

#[test]
fn version_json_uses_stable_envelope() {
    let output = Command::cargo_bin("demoswarm")
        .expect("binary exists")
        .args(["--json", "version"])
        .output()
        .expect("command runs");
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["command"], "version");
    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["manager"]["package"], "demoswarm");
}

#[test]
fn configure_dry_run_writes_nothing() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    Command::cargo_bin("demoswarm")
        .expect("binary exists")
        .args([
            "--project",
            temporary.path().to_str().expect("UTF-8 path"),
            "--dry-run",
            "configure",
            "--platform",
            "claude-code",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("no files were written"));
    assert!(!temporary.path().join(".demoswarm/config.toml").exists());
}

#[test]
fn configure_creates_project_owned_config_once() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().to_str().expect("UTF-8 path");
    Command::cargo_bin("demoswarm")
        .expect("binary exists")
        .args(["--project", root, "configure", "--platform", "codex"])
        .assert()
        .success();
    let config_path = temporary.path().join(".demoswarm/config.toml");
    let content = fs::read_to_string(&config_path).expect("config readable");
    assert!(content.contains("schema_version = 1"));
    assert!(content.contains("[platforms.codex]"));

    Command::cargo_bin("demoswarm")
        .expect("binary exists")
        .args(["--project", root, "configure"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already exists"));
}

#[test]
fn lifecycle_commands_fail_without_claiming_mutation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let output = Command::cargo_bin("demoswarm")
        .expect("binary exists")
        .args([
            "--project",
            temporary.path().to_str().expect("UTF-8 path"),
            "--json",
            "install",
            "--platform",
            "claude-code",
        ])
        .output()
        .expect("command runs");
    assert_eq!(output.status.code(), Some(4));
    let value: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(value["ok"], false);
    assert_eq!(value["data"]["side_effects_performed"], false);
}

#[test]
fn runs_validate_checks_cross_file_identity() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let run_dir = temporary.path().join(".runs/gh-1/signal");
    fs::create_dir_all(&run_dir).expect("run directories");
    fs::write(
        temporary.path().join(".runs/gh-1/run.json"),
        r#"{"schema_version":"1.0","run_id":"gh-1"}"#,
    )
    .expect("manifest written");
    fs::write(
        run_dir.join("receipt.json"),
        r#"{"schema_version":"2.0","run_id":"gh-1","flow":"signal","completion":"COMPLETE","verification":"VERIFIED"}"#,
    )
    .expect("receipt written");

    Command::cargo_bin("demoswarm")
        .expect("binary exists")
        .args([
            "--project",
            temporary.path().to_str().expect("UTF-8 path"),
            "runs",
            "validate",
            "gh-1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 error"));
}
