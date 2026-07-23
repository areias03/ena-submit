//! End-to-end tests that drive the compiled `ena-submit` binary.
//!
//! These stay offline: every path exercised here fails or scaffolds before any network call to the
//! ENA taxonomy service would happen.

use std::path::Path;
use std::process::{Command, Output};

/// A `Command` for the freshly built binary (path injected by Cargo for integration tests).
fn ena_submit(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ena-submit"));
    cmd.current_dir(dir);
    cmd
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn init_scaffolds_config_and_templates() {
    let dir = tempfile::tempdir().unwrap();
    let out = ena_submit(dir.path()).arg("init").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    assert!(dir.path().join("ena-submit.toml").exists());
    for template in [
        "templates/reads.tsv",
        "templates/assemblies.tsv",
        "templates/mag_samples.tsv",
        "templates/mag_assemblies.tsv",
        "templates/registered_mags.tsv",
    ] {
        assert!(dir.path().join(template).exists(), "missing {template}");
    }
}

#[test]
fn init_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    ena_submit(dir.path()).arg("init").output().unwrap();

    // A second run must succeed and leave existing files untouched, reporting them as skipped.
    let out = ena_submit(dir.path()).arg("init").output().unwrap();
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("skipped"), "stdout: {}", stdout(&out));
}

#[test]
fn mag_prepare_missing_scientific_name_errors_offline() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("mags.tsv");
    // No `scientific_name` column: rejected before any taxonomy lookup.
    std::fs::write(&input, "sample_alias\ttax_id\nbin.1\t\n").unwrap();

    let out = ena_submit(dir.path())
        .args(["mag", "prepare"])
        .arg(&input)
        .args(["-o", "out.tsv"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("missing required column: scientific_name"),
        "stderr: {}",
        stderr(&out)
    );
    assert!(!dir.path().join("out.tsv").exists(), "must not write output on error");
}

#[test]
fn submission_commands_report_not_implemented() {
    let dir = tempfile::tempdir().unwrap();
    // The `reads` arm returns before reading the input, so its contents are irrelevant.
    let out = ena_submit(dir.path())
        .args(["reads", "reads.tsv", "--validate"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("not yet implemented"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn unknown_command_is_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = ena_submit(dir.path()).arg("frobnicate").output().unwrap();
    // clap rejects unknown subcommands with a non-zero exit and usage on stderr.
    assert!(!out.status.success());
    assert!(stderr(&out).to_lowercase().contains("usage"), "stderr: {}", stderr(&out));
}
