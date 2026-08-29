use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn compatibility_binary_calls_the_canonical_library() {
    Command::cargo_bin("demo-swarm")
        .expect("compatibility binary exists")
        .args(["version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("demoswarm"));
}
