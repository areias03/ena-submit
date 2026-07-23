//! Append-only local submission history (`.ena-submit/history.jsonl`).
//!
//! Every submission attempt — validate-only or real, success or failure — is recorded as one JSON
//! object on its own line, appended and never rewritten (see
//! [ADR 0007](../../docs/adr/0007-append-only-jsonl-history.md)). `ena-submit status` reads the file
//! back and renders it; the submission layer (milestone 7) is the only writer.
//!
//! Reading is forward-compatible: unknown fields are ignored, so newer records stay readable by
//! older builds. A record whose line cannot be parsed at all surfaces as an [`Error::Input`] naming
//! the offending line rather than being silently dropped.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Error, Result};
use crate::model::{Accession, Context, Environment, SubmitMode};

/// Location of the history file, relative to the working directory.
pub const HISTORY_FILE: &str = ".ena-submit/history.jsonl";

/// One recorded submission attempt. Extra fields a future version might add are ignored on read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// When the attempt was made, RFC 3339 / ISO 8601 in UTC.
    pub timestamp: String,
    /// Webin-CLI context the object was submitted under.
    pub context: Context,
    /// The object's name (`NAME` for reads, `ASSEMBLYNAME` for genomes).
    pub name: String,
    /// Whether this was validate-only or a real submission.
    pub mode: SubmitMode,
    /// Which Webin service was targeted.
    pub environment: Environment,
    /// Whether the attempt succeeded.
    pub outcome: Outcome,
    /// Accessions minted by a real submission (empty for validate-only or failed attempts).
    #[serde(default)]
    pub accessions: Vec<Accession>,
    /// Path to the Webin-CLI receipt XML, when one was written.
    #[serde(default)]
    pub receipt: Option<PathBuf>,
    /// Failure detail when `outcome` is [`Outcome::Failure`].
    #[serde(default)]
    pub error: Option<String>,
}

impl Record {
    /// Start a record for `name` in `context`/`mode`/`environment`, timestamped now, with no outcome
    /// yet — call [`Record::succeeded`] or [`Record::failed`] to finalize it.
    pub fn now(context: Context, name: impl Into<String>, mode: SubmitMode, env: Environment) -> Self {
        Record {
            timestamp: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .unwrap_or_default(),
            context,
            name: name.into(),
            mode,
            environment: env,
            outcome: Outcome::Failure,
            accessions: Vec::new(),
            receipt: None,
            error: None,
        }
    }

    /// Mark the attempt successful, attaching any minted accessions and the receipt path.
    pub fn succeeded(mut self, accessions: Vec<Accession>, receipt: Option<PathBuf>) -> Self {
        self.outcome = Outcome::Success;
        self.accessions = accessions;
        self.receipt = receipt;
        self
    }

    /// Mark the attempt failed with a human-readable reason.
    pub fn failed(mut self, error: impl Into<String>) -> Self {
        self.outcome = Outcome::Failure;
        self.error = Some(error.into());
        self
    }
}

/// Outcome of a submission attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Success,
    Failure,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Success => "ok",
            Outcome::Failure => "FAILED",
        }
    }
}

/// The append-only history at a fixed path.
pub struct History {
    path: PathBuf,
}

impl History {
    /// The history rooted in working directory `dir` (`<dir>/.ena-submit/history.jsonl`).
    pub fn at(dir: &Path) -> Self {
        History {
            path: dir.join(HISTORY_FILE),
        }
    }

    /// Append one record as a JSON line, creating the parent directory if needed.
    pub fn append(&self, record: &Record) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let mut line = serde_json::to_string(record).map_err(|e| Error::Input {
            path: self.path.clone(),
            message: format!("could not serialize history record: {e}"),
        })?;
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| Error::io(&self.path, e))?;
        file.write_all(line.as_bytes())
            .map_err(|e| Error::io(&self.path, e))?;
        Ok(())
    }

    /// Read every record in file order. A missing file is an empty history, not an error.
    pub fn read(&self) -> Result<Vec<Record>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&self.path, e)),
        };
        let mut records = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let record = serde_json::from_str::<Record>(line).map_err(|e| Error::Input {
                path: self.path.clone(),
                message: format!("malformed history record on line {}: {e}", i + 1),
            })?;
            records.push(record);
        }
        Ok(records)
    }
}

/// Render the history as human-readable text for `ena-submit status`.
pub fn render(records: &[Record]) -> String {
    if records.is_empty() {
        return "No submissions recorded yet.\n".to_string();
    }
    let mut out = String::new();
    for r in records {
        out.push_str(&format!(
            "{}  {:<10} {:<8} {:<6} {:<6} {}",
            r.timestamp,
            r.environment,
            r.mode.flag(),
            r.context,
            r.outcome.label(),
            r.name,
        ));
        if !r.accessions.is_empty() {
            let accs: Vec<String> = r
                .accessions
                .iter()
                .map(|a| format!("{}={}", a.kind, a.accession))
                .collect();
            out.push_str(&format!("  [{}]", accs.join(", ")));
        }
        if let Some(err) = &r.error {
            out.push_str(&format!("  ({err})"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str) -> Record {
        Record::now(Context::Genome, name, SubmitMode::Submit, Environment::Test)
    }

    #[test]
    fn append_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::at(dir.path());

        let a = record("asm1").succeeded(
            vec![Accession {
                kind: "ANALYSIS".into(),
                accession: "ERZ1".into(),
            }],
            Some(PathBuf::from("receipt.xml")),
        );
        let b = record("asm2").failed("validation error");
        history.append(&a).unwrap();
        history.append(&b).unwrap();

        let back = history.read().unwrap();
        assert_eq!(back, vec![a, b]);
    }

    #[test]
    fn missing_file_is_empty_history() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::at(dir.path());
        assert!(history.read().unwrap().is_empty());
    }

    #[test]
    fn append_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::at(dir.path());
        // The `.ena-submit/` directory does not exist yet.
        assert!(!dir.path().join(".ena-submit").exists());
        history.append(&record("asm1").succeeded(vec![], None)).unwrap();
        assert!(dir.path().join(HISTORY_FILE).exists());
    }

    #[test]
    fn each_record_is_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::at(dir.path());
        history.append(&record("a").succeeded(vec![], None)).unwrap();
        history.append(&record("b").failed("boom")).unwrap();
        let text = std::fs::read_to_string(dir.path().join(HISTORY_FILE)).unwrap();
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn malformed_line_names_the_line() {
        let dir = tempfile::tempdir().unwrap();
        let history = History::at(dir.path());
        history.append(&record("a").succeeded(vec![], None)).unwrap();
        // Corrupt the store with a non-JSON second line.
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.path().join(HISTORY_FILE))
            .unwrap();
        file.write_all(b"not json\n").unwrap();

        let err = history.read().unwrap_err().to_string();
        assert!(err.contains("line 2"), "got: {err}");
    }

    #[test]
    fn unknown_fields_are_ignored_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(HISTORY_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A record carrying a field this build doesn't know about must still parse.
        std::fs::write(
            &path,
            r#"{"timestamp":"2026-07-23T00:00:00Z","context":"reads","name":"run1","mode":"validate","environment":"test","outcome":"success","future_field":42}"#,
        )
        .unwrap();
        let back = History::at(dir.path()).read().unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "run1");
        assert_eq!(back[0].context, Context::Reads);
    }

    #[test]
    fn render_empty_history() {
        assert!(render(&[]).contains("No submissions recorded yet"));
    }

    #[test]
    fn render_includes_name_outcome_and_accessions() {
        let r = record("asm1").succeeded(
            vec![Accession {
                kind: "ANALYSIS".into(),
                accession: "ERZ1".into(),
            }],
            None,
        );
        let text = render(&[r]);
        assert!(text.contains("asm1"), "got: {text}");
        assert!(text.contains("ok"), "got: {text}");
        assert!(text.contains("ANALYSIS=ERZ1"), "got: {text}");
        assert!(text.contains("genome"), "got: {text}");
    }

    #[test]
    fn render_shows_failure_reason() {
        let text = render(&[record("asm1").failed("bad checklist")]);
        assert!(text.contains("FAILED"), "got: {text}");
        assert!(text.contains("bad checklist"), "got: {text}");
    }
}
