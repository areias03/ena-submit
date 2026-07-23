//! Invoke the official Java Webin-CLI and interpret its result.
//!
//! `ena-submit` is a wrapper: it renders a manifest, then shells out to
//! `java -jar webin-cli.jar -context … -manifest …` to do the actual validation and transfer (see
//! [ADR 0002](../../docs/adr/0002-wrap-webin-cli-hybrid.md)). This module builds the argument list,
//! preflights the toolchain (Java 17+ and the jar), runs one object, and — for real submissions —
//! reads back the receipt XML to collect accessions. Each attempt is turned into a history
//! [`Record`] so the caller can append it and report a summary.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::history::Record;
use crate::model::{Context, Environment, SubmitMode};
use crate::receipt;

/// Minimum Java major version Webin-CLI requires.
const MIN_JAVA_MAJOR: u32 = 17;

/// Webin-CLI's run-level report, rewritten under `-outputDir` on every invocation.
const RUN_REPORT: &str = "webin-cli.report";

/// The message Webin-CLI writes to its run report when Webin rejects the submission account.
/// Credentials cannot be checked before invoking it (there is no separate auth step), so a rejected
/// account is detected here and turned into a run-level abort.
const AUTH_FAILURE_MARKER: &str = "Invalid submission account user name or password";

/// The fixed parameters of one Webin-CLI invocation (everything except credentials).
pub struct WebinRun<'a> {
    pub context: Context,
    /// The object's name (`NAME`/`ASSEMBLYNAME`), used to locate its receipt.
    pub name: &'a str,
    /// Rendered manifest text for this object.
    pub manifest: &'a str,
    pub mode: SubmitMode,
    pub environment: Environment,
    /// Directory the manifest's file paths are relative to (Webin-CLI `-inputDir`), if any.
    pub input_dir: Option<&'a Path>,
}

/// Credentials checked once per run by [`preflight`] and handed to every [`submit_object`] call,
/// so the "are we authenticated?" question is answered in exactly one place.
#[derive(Clone, Copy)]
pub struct Credentials<'a> {
    pub username: &'a str,
    pub password: &'a str,
}

/// Check everything a submission run needs before touching any input: credentials (Webin-CLI
/// authenticates even for validate-only runs), the jar, and a new-enough Java. Called once per run
/// so a missing credential or toolchain fails fast with a clear message.
pub fn preflight(cfg: &Config) -> Result<Credentials<'_>> {
    let (username, password) = cfg.require_credentials()?;
    if !cfg.webin_cli_jar.exists() {
        return Err(Error::Config(format!(
            "Webin-CLI jar not found at {} — download it from \
             https://github.com/enasequence/webin-cli/releases/latest or set webin_cli_jar",
            cfg.webin_cli_jar.display()
        )));
    }
    let version_output = Command::new(&cfg.java_bin)
        .arg("-version")
        .output()
        .map_err(|e| {
            Error::Config(format!(
                "could not run Java ('{}': {e}) — install Java {MIN_JAVA_MAJOR}+ or set java_bin",
                cfg.java_bin.display()
            ))
        })?;
    // `java -version` prints to stderr.
    let text = String::from_utf8_lossy(&version_output.stderr);
    match parse_java_major(&text) {
        Some(major) if major >= MIN_JAVA_MAJOR => Ok(Credentials { username, password }),
        Some(major) => Err(Error::Config(format!(
            "Java {major} is too old; Webin-CLI needs Java {MIN_JAVA_MAJOR}+"
        ))),
        None => Err(Error::Config(format!(
            "could not determine Java version from: {}",
            text.trim()
        ))),
    }
}

/// Submit or validate one object end-to-end: write its manifest, invoke Webin-CLI, and (for a real
/// submission) read the receipt. Returns a history [`Record`] describing the outcome.
///
/// A per-object validation/submission *failure* is captured in the returned `Record` (so the caller
/// records it and moves on). Run-level failures — cannot write the manifest, cannot spawn the
/// process, or Webin rejected the account — are returned as `Err` and abort the run.
pub fn submit_object(cfg: &Config, run: &WebinRun, creds: Credentials<'_>) -> Result<Record> {
    let manifest_path = write_manifest(cfg, run.context, run.name, run.manifest)?;
    let args = build_args(cfg, run, &manifest_path, creds);

    // Drop any report left by an earlier invocation so what we read back is definitely this run's.
    clear_run_report(cfg);

    tracing::info!(name = run.name, context = %run.context, mode = run.mode.flag(), "invoking webin-cli");
    let status = Command::new(&cfg.java_bin)
        .args(&args)
        .status()
        .map_err(|e| {
            Error::Config(format!(
                "failed to run '{}' -jar {}: {e}",
                cfg.java_bin.display(),
                cfg.webin_cli_jar.display()
            ))
        })?;

    // A rejected account is not this object's fault and would repeat for every remaining one, so
    // abort the whole run with a clear message instead of recording N identical failures.
    if !status.success() && auth_was_rejected(cfg) {
        return Err(Error::InvalidCredentials);
    }

    let record = Record::now(run.context, run.name, run.mode, run.environment);
    Ok(interpret(cfg, run, status.success(), record))
}

/// Path of Webin-CLI's run-level report under the configured output directory.
fn run_report_path(cfg: &Config) -> PathBuf {
    cfg.output_dir.join(RUN_REPORT)
}

/// Remove a stale run report, ignoring a missing file (and any other error: this is best-effort
/// hygiene, and [`auth_was_rejected`] is only consulted after a failing run anyway).
fn clear_run_report(cfg: &Config) {
    let _ = std::fs::remove_file(run_report_path(cfg));
}

/// Whether Webin-CLI's run report says the submission account was rejected. An unreadable or absent
/// report means "not an auth failure" — the object-level failure path then applies as before.
fn auth_was_rejected(cfg: &Config) -> bool {
    match std::fs::read_to_string(run_report_path(cfg)) {
        Ok(text) => text.contains(AUTH_FAILURE_MARKER),
        Err(_) => false,
    }
}

/// Turn Webin-CLI's exit status into a finalized history [`Record`], reading the receipt on a real
/// submission to collect accessions (success) or an error summary (failure).
fn interpret(cfg: &Config, run: &WebinRun, exit_ok: bool, record: Record) -> Record {
    match run.mode {
        SubmitMode::Validate => {
            if exit_ok {
                record.succeeded(Vec::new(), None)
            } else {
                record.failed("validation failed (see Webin-CLI output above)")
            }
        }
        SubmitMode::Submit => {
            let path = receipt_path(&cfg.output_dir, run.context, run.name);
            let receipt = receipt::read_receipt(&path);
            // The process exit status is authoritative: a non-zero exit is a failure even if a
            // (possibly stale) receipt from an earlier run is still on disk. Only when Webin-CLI
            // exited cleanly do we trust the receipt to confirm success and carry the accessions.
            if !exit_ok {
                let reason = match receipt {
                    Ok(r) if !r.errors.is_empty() => r.error_summary(),
                    _ => "submission failed (see Webin-CLI output above)".to_string(),
                };
                return record.failed(reason);
            }
            match receipt {
                Ok(r) if r.success => record.succeeded(r.accessions, Some(path)),
                Ok(r) => record.failed(r.error_summary()),
                Err(e) => record.failed(format!(
                    "submission reported success but the receipt could not be read: {e}"
                )),
            }
        }
    }
}

/// Build the full Webin-CLI argument vector (including `-jar <jar>` and credentials).
fn build_args(
    cfg: &Config,
    run: &WebinRun,
    manifest_path: &Path,
    creds: Credentials<'_>,
) -> Vec<String> {
    let mut args = vec![
        "-jar".to_string(),
        cfg.webin_cli_jar.to_string_lossy().into_owned(),
        "-context".to_string(),
        run.context.as_str().to_string(),
        "-userName".to_string(),
        creds.username.to_string(),
        "-password".to_string(),
        creds.password.to_string(),
        "-manifest".to_string(),
        manifest_path.to_string_lossy().into_owned(),
        "-outputDir".to_string(),
        cfg.output_dir.to_string_lossy().into_owned(),
    ];
    if let Some(dir) = run.input_dir {
        args.push("-inputDir".to_string());
        args.push(dir.to_string_lossy().into_owned());
    }
    args.push(format!("-{}", run.mode.flag()));
    if run.environment.is_test() {
        args.push("-test".to_string());
    }
    args
}

/// Write `manifest` text to `<output_dir>/manifests/<name>.manifest` and return its path.
fn write_manifest(cfg: &Config, context: Context, name: &str, manifest: &str) -> Result<PathBuf> {
    let dir = cfg.output_dir.join("manifests");
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let path = dir.join(format!("{}_{}.manifest", context.as_str(), sanitize(name)));
    std::fs::write(&path, manifest).map_err(|e| Error::io(&path, e))?;
    Ok(path)
}

/// Where Webin-CLI writes the receipt for `name` under `context`: `<out>/<context>/<name>/submit/`.
/// Webin-CLI sanitizes the name into the directory component (spaces and punctuation → `_`), so we
/// must apply the same [`sanitize`] here to locate the receipt for names containing such characters.
pub fn receipt_path(output_dir: &Path, context: Context, name: &str) -> PathBuf {
    output_dir
        .join(context.as_str())
        .join(sanitize(name))
        .join("submit")
        .join("receipt.xml")
}

/// Make `name` safe as a file-name component (spaces and other punctuation → `_`).
pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
            c
        } else {
            '_'
        })
        .collect()
}

/// Extract the major version from `java -version` output. Handles both the legacy `1.8` scheme
/// (major is the second component) and the modern one (`17`, `21`, `17.0.9`, `21-ea`).
fn parse_java_major(text: &str) -> Option<u32> {
    // Find the quoted version string after the word `version`.
    let start = text.find("version")?;
    let after = &text[start..];
    let quote = after.find('"')?;
    let rest = &after[quote + 1..];
    let end = rest.find('"')?;
    let version = &rest[..end];

    let mut parts = version.split('.');
    let first = parts.next()?;
    if first == "1" {
        // Legacy "1.8.0_281" -> major 8.
        leading_number(parts.next()?)
    } else {
        // Modern "17", "21-ea", "17.0.9" -> major 17/21/17.
        leading_number(first)
    }
}

/// Parse the leading run of ASCII digits (e.g. `17-ea` -> 17).
fn leading_number(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &Path) -> Config {
        Config {
            webin_username: Some("Webin-1".into()),
            webin_password: Some("secret".into()),
            default_environment: Environment::Test,
            webin_cli_jar: dir.join("webin-cli.jar"),
            java_bin: PathBuf::from("java"),
            output_dir: dir.join("out"),
        }
    }

    fn creds<'a>(username: &'a str, password: &'a str) -> Credentials<'a> {
        Credentials { username, password }
    }

    #[test]
    fn build_args_has_core_flags_for_validate_test() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let run = WebinRun {
            context: Context::Genome,
            name: "asm1",
            manifest: "STUDY\tX\n",
            mode: SubmitMode::Validate,
            environment: Environment::Test,
            input_dir: None,
        };
        let args = build_args(&cfg, &run, Path::new("m.manifest"), creds("Webin-1", "secret"));
        assert!(args.windows(2).any(|w| w == ["-context", "genome"]));
        assert!(args.windows(2).any(|w| w == ["-userName", "Webin-1"]));
        assert!(args.windows(2).any(|w| w == ["-password", "secret"]));
        assert!(args.windows(2).any(|w| w == ["-manifest", "m.manifest"]));
        assert!(args.iter().any(|a| a == "-validate"));
        assert!(args.iter().any(|a| a == "-test"));
        assert!(!args.iter().any(|a| a == "-submit"));
        assert!(!args.iter().any(|a| a == "-inputDir"));
    }

    #[test]
    fn build_args_submit_production_with_input_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let run = WebinRun {
            context: Context::Reads,
            name: "run1",
            manifest: "",
            mode: SubmitMode::Submit,
            environment: Environment::Production,
            input_dir: Some(Path::new("data")),
        };
        let args = build_args(&cfg, &run, Path::new("m.manifest"), creds("u", "p"));
        assert!(args.iter().any(|a| a == "-submit"));
        assert!(!args.iter().any(|a| a == "-test"));
        assert!(args.windows(2).any(|w| w == ["-inputDir", "data"]));
        assert!(args.windows(2).any(|w| w == ["-context", "reads"]));
    }

    #[test]
    fn receipt_path_layout() {
        let p = receipt_path(Path::new("out"), Context::Genome, "asm1");
        assert!(p.ends_with("out/genome/asm1/submit/receipt.xml"));
    }

    #[test]
    fn receipt_path_sanitizes_name_to_match_webin_output_dir() {
        let p = receipt_path(Path::new("out"), Context::Genome, "MAG bin.1");
        assert!(p.ends_with("out/genome/MAG_bin.1/submit/receipt.xml"), "got: {}", p.display());
    }

    #[test]
    fn sanitize_replaces_spaces_and_punctuation() {
        assert_eq!(sanitize("MAG bin/1"), "MAG_bin_1");
        assert_eq!(sanitize("asm_1.v3-final"), "asm_1.v3-final");
    }

    #[test]
    fn parses_modern_and_legacy_java_versions() {
        assert_eq!(parse_java_major("openjdk version \"17.0.9\" 2023-10-17"), Some(17));
        assert_eq!(parse_java_major("openjdk version \"21\" 2023-09-19"), Some(21));
        assert_eq!(parse_java_major("openjdk version \"21-ea\" 2023"), Some(21));
        assert_eq!(parse_java_major("java version \"1.8.0_281\""), Some(8));
        assert_eq!(parse_java_major("java version \"11.0.2\" 2019-01-15"), Some(11));
        assert_eq!(parse_java_major("no version here"), None);
    }

    #[test]
    fn preflight_reports_missing_jar() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path()); // jar path does not exist
        // `Credentials` is deliberately not `Debug` (it holds a password), so match instead of unwrap.
        match preflight(&cfg) {
            Err(e) => assert!(e.to_string().contains("jar not found"), "got: {e}"),
            Ok(_) => panic!("expected a missing-jar error"),
        }
    }

    #[test]
    fn preflight_requires_credentials_before_touching_the_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path());
        cfg.webin_password = None;
        // Credentials are checked first, so this fails on the account, not the (also missing) jar.
        assert!(matches!(
            preflight(&cfg),
            Err(Error::MissingCredentials)
        ));
    }

    #[test]
    fn auth_rejection_is_detected_from_the_run_report() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        std::fs::create_dir_all(&cfg.output_dir).unwrap();
        let report = run_report_path(&cfg);

        // No report at all (Webin-CLI died before writing one) is not an auth failure.
        assert!(!auth_was_rejected(&cfg));

        // A run that failed for some other reason must stay an object-level failure.
        std::fs::write(&report, "2026-07-24T09:00:00 ERROR: Invalid manifest field 'STUDY'\n").unwrap();
        assert!(!auth_was_rejected(&cfg));

        // The real Webin-CLI message, as written to the report alongside its stack trace.
        std::fs::write(
            &report,
            "2026-07-24T09:00:00 ERROR: Invalid submission account user name or password. \
             Please try enclosing your password in single quotes.\n\
             uk.ac.ebi.ena.webin.cli.WebinCliException: Invalid submission account user name or \
             password.\n\tat uk.ac.ebi.ena.webin.cli.service.LoginService.login(LoginService.java:74)\n",
        )
        .unwrap();
        assert!(auth_was_rejected(&cfg));

        // Clearing it before an invocation means a stale report cannot abort a later good run.
        clear_run_report(&cfg);
        assert!(!report.exists());
        assert!(!auth_was_rejected(&cfg));
    }

    #[test]
    fn clear_run_report_tolerates_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path()); // output_dir does not even exist yet
        clear_run_report(&cfg); // must not panic
    }

    #[test]
    fn write_manifest_creates_file_under_output_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let path = write_manifest(&cfg, Context::Genome, "MAG bin 1", "STUDY\tX\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "STUDY\tX\n");
        assert!(path.to_string_lossy().contains("genome_MAG_bin_1.manifest"));
    }

    #[test]
    fn interpret_validate_success_and_failure() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let run = WebinRun {
            context: Context::Genome,
            name: "asm1",
            manifest: "",
            mode: SubmitMode::Validate,
            environment: Environment::Test,
            input_dir: None,
        };
        let ok = interpret(&cfg, &run, true, Record::now(run.context, run.name, run.mode, run.environment));
        assert_eq!(ok.outcome, crate::history::Outcome::Success);

        let bad = interpret(&cfg, &run, false, Record::now(run.context, run.name, run.mode, run.environment));
        assert_eq!(bad.outcome, crate::history::Outcome::Failure);
    }

    #[test]
    fn interpret_submit_reads_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let run = WebinRun {
            context: Context::Genome,
            name: "asm1",
            manifest: "",
            mode: SubmitMode::Submit,
            environment: Environment::Test,
            input_dir: None,
        };
        // Seed a successful receipt where Webin-CLI would write it.
        let rp = receipt_path(&cfg.output_dir, run.context, run.name);
        std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
        std::fs::write(
            &rp,
            r#"<RECEIPT success="true"><ANALYSIS accession="ERZ1"/></RECEIPT>"#,
        )
        .unwrap();

        let rec = interpret(&cfg, &run, true, Record::now(run.context, run.name, run.mode, run.environment));
        assert_eq!(rec.outcome, crate::history::Outcome::Success);
        assert_eq!(rec.accessions.len(), 1);
        assert_eq!(rec.accessions[0].accession, "ERZ1");
        assert_eq!(rec.receipt, Some(rp));
    }

    #[test]
    fn interpret_submit_nonzero_exit_is_failure_despite_stale_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let run = WebinRun {
            context: Context::Genome,
            name: "asm1",
            manifest: "",
            mode: SubmitMode::Submit,
            environment: Environment::Test,
            input_dir: None,
        };
        // A leftover successful receipt from an earlier run must not mask a non-zero exit.
        let rp = receipt_path(&cfg.output_dir, run.context, run.name);
        std::fs::create_dir_all(rp.parent().unwrap()).unwrap();
        std::fs::write(&rp, r#"<RECEIPT success="true"><ANALYSIS accession="ERZ1"/></RECEIPT>"#).unwrap();

        let rec = interpret(&cfg, &run, false, Record::now(run.context, run.name, run.mode, run.environment));
        assert_eq!(rec.outcome, crate::history::Outcome::Failure);
        assert!(rec.accessions.is_empty());
    }

    #[test]
    fn interpret_submit_missing_receipt_is_failure() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path());
        let run = WebinRun {
            context: Context::Genome,
            name: "asm1",
            manifest: "",
            mode: SubmitMode::Submit,
            environment: Environment::Test,
            input_dir: None,
        };
        let rec = interpret(&cfg, &run, true, Record::now(run.context, run.name, run.mode, run.environment));
        assert_eq!(rec.outcome, crate::history::Outcome::Failure);
    }
}
