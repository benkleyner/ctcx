use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn cli_runs_init_validate_check_explain_and_compile_alias() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("ctcx.yaml");

    Command::cargo_bin("ctcx")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("AGENTS.md"));

    Command::cargo_bin("ctcx")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "validate"])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid: 2 output(s)"));

    Command::cargo_bin("ctcx")
        .unwrap()
        .current_dir(temp.path().join("instructions"))
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::contains("up to date"));

    Command::cargo_bin("ctcx")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "explain",
            "--target",
            "agents",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied project-workflow"));

    Command::cargo_bin("ctcx")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "compile", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("up to date"));
}

#[test]
fn cli_returns_failure_for_drift_and_prints_a_diff() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("ctcx.yaml");
    Command::cargo_bin("ctcx")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "init"])
        .assert()
        .success();
    std::fs::write(temp.path().join("AGENTS.md"), "manual\n").unwrap();

    Command::cargo_bin("ctcx")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "check",
            "--target",
            "agents",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("modified output agents"));

    Command::cargo_bin("ctcx")
        .unwrap()
        .args([
            "--config",
            config.to_str().unwrap(),
            "diff",
            "--target",
            "agents",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("--- a/AGENTS.md"));
}

#[test]
fn cli_validate_reports_aggregated_guardrail_failures() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("ctcx.yaml");
    Command::cargo_bin("ctcx")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "init"])
        .assert()
        .success();
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(
        r#"    checks:
      - type: package-script
        manifest: package.json
        script: test
      - type: path-exists
        path: scripts/setup.sh
        kind: file
"#,
    );
    std::fs::write(&config, text).unwrap();

    Command::cargo_bin("ctcx")
        .unwrap()
        .args(["--config", config.to_str().unwrap(), "validate"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("guardrail validation failed:")
                .and(predicate::str::contains("targets [agents, claude]"))
                .and(predicate::str::contains(
                    "package manifest package.json does not exist",
                ))
                .and(predicate::str::contains(
                    "required path scripts/setup.sh does not exist",
                )),
        );
}
