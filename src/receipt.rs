//! Parse the receipt XML that Webin-CLI writes after a submission.
//!
//! A receipt is an `<RECEIPT>` element with `success="true|false"`, zero or more accessioned child
//! elements (`ANALYSIS`, `RUN`, `EXPERIMENT`, `SUBMISSION`, … — each carrying an `accession`
//! attribute), and a `<MESSAGES>` block of `<INFO>` / `<ERROR>` text. We collect the accessions and
//! any error/info messages so the submission layer can record them in the history and surface
//! failures. Validate-only runs write no receipt; only real submissions produce one.

use std::path::Path;

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::error::{Error, Result};
use crate::model::Accession;

/// The parsed contents of a Webin-CLI receipt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Receipt {
    /// ENA's overall `success` flag for the submission.
    pub success: bool,
    /// Every accessioned object in the receipt, in document order.
    pub accessions: Vec<Accession>,
    /// `<ERROR>` messages (present when `success` is false).
    pub errors: Vec<String>,
    /// `<INFO>` messages.
    pub info: Vec<String>,
}

impl Receipt {
    /// A single string summarizing the receipt's error messages, or a generic fallback.
    pub fn error_summary(&self) -> String {
        if self.errors.is_empty() {
            "submission was not successful (no error message in receipt)".to_string()
        } else {
            self.errors.join("; ")
        }
    }
}

/// Read and parse a receipt XML file.
pub fn read_receipt(path: &Path) -> Result<Receipt> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    parse_receipt(&text).map_err(|message| Error::Input {
        path: path.to_path_buf(),
        message,
    })
}

/// Parse receipt XML text. On malformed XML returns the parser's message (the caller tags it with a
/// path). Any element bearing an `accession` attribute is recorded, keyed by its element name.
pub fn parse_receipt(xml: &str) -> std::result::Result<Receipt, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut receipt = Receipt::default();
    // Which message bucket the text we next see belongs to, if any.
    let mut in_message: Option<Message> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let name = element_name(e.name().as_ref());
                match name.as_str() {
                    "RECEIPT" => receipt.success = attr_equals(&e, b"success", "true"),
                    "ERROR" => in_message = Some(Message::Error),
                    "INFO" => in_message = Some(Message::Info),
                    _ => record_accession(&mut receipt, name, &e),
                }
            }
            Ok(Event::Empty(e)) => {
                // A self-closing element (e.g. `<INFO/>`) produces no matching `End`, so it must not
                // enter a message bucket — doing so would leak `in_message` and misattribute a later
                // element's text. It has no text children, so only its attributes matter here.
                let name = element_name(e.name().as_ref());
                match name.as_str() {
                    "RECEIPT" => receipt.success = attr_equals(&e, b"success", "true"),
                    "ERROR" | "INFO" => {}
                    _ => record_accession(&mut receipt, name, &e),
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(bucket) = in_message {
                    let text = t.unescape().map_err(|e| e.to_string())?.trim().to_string();
                    if !text.is_empty() {
                        match bucket {
                            Message::Error => receipt.errors.push(text),
                            Message::Info => receipt.info.push(text),
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = element_name(e.name().as_ref());
                if name == "ERROR" || name == "INFO" {
                    in_message = None;
                }
            }
            Ok(_) => {}
            Err(e) => return Err(format!("malformed receipt XML: {e}")),
        }
    }
    Ok(receipt)
}

#[derive(Clone, Copy)]
enum Message {
    Error,
    Info,
}

/// Record the element as an accession if it carries an `accession` attribute.
fn record_accession(receipt: &mut Receipt, kind: String, e: &quick_xml::events::BytesStart) {
    if let Some(accession) = attr_value(e, b"accession") {
        receipt.accessions.push(Accession { kind, accession });
    }
}

/// Uppercased element local name as a `String` (namespaces are not expected in receipts).
fn element_name(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).to_ascii_uppercase()
}

/// The value of attribute `key` on `e`, if present.
fn attr_value(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        if a.key.as_ref() == key {
            a.unescape_value().ok().map(|v| v.into_owned())
        } else {
            None
        }
    })
}

/// Whether attribute `key` on `e` equals `expected` (case-insensitive).
fn attr_equals(e: &quick_xml::events::BytesStart, key: &[u8], expected: &str) -> bool {
    attr_value(e, key).is_some_and(|v| v.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENOME_OK: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<RECEIPT receiptDate="2026-07-23T10:00:00.000Z" submissionFile="submission.xml" success="true">
  <ANALYSIS accession="ERZ1234567" alias="webin-genome-asm1" status="PRIVATE"/>
  <SUBMISSION accession="ERA9999999" alias="webin-genome-asm1"/>
  <MESSAGES>
    <INFO>The submission has been processed successfully.</INFO>
  </MESSAGES>
  <ACTIONS>ADD</ACTIONS>
</RECEIPT>"#;

    const READS_OK: &str = r#"<RECEIPT success="true">
  <EXPERIMENT accession="ERX1" alias="exp"/>
  <RUN accession="ERR1" alias="run"/>
  <SUBMISSION accession="ERA1"/>
</RECEIPT>"#;

    const FAILURE: &str = r#"<RECEIPT success="false">
  <MESSAGES>
    <ERROR>In sample, checklist ERC000050 is not valid.</ERROR>
    <ERROR>Sample accession is missing.</ERROR>
  </MESSAGES>
</RECEIPT>"#;

    #[test]
    fn parses_successful_genome_receipt() {
        let r = parse_receipt(GENOME_OK).unwrap();
        assert!(r.success);
        assert_eq!(
            r.accessions,
            vec![
                Accession {
                    kind: "ANALYSIS".into(),
                    accession: "ERZ1234567".into()
                },
                Accession {
                    kind: "SUBMISSION".into(),
                    accession: "ERA9999999".into()
                },
            ]
        );
        assert_eq!(r.info, ["The submission has been processed successfully."]);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn parses_reads_accessions() {
        let r = parse_receipt(READS_OK).unwrap();
        assert!(r.success);
        let kinds: Vec<&str> = r.accessions.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(kinds, ["EXPERIMENT", "RUN", "SUBMISSION"]);
        assert_eq!(r.accessions[1].accession, "ERR1");
    }

    #[test]
    fn parses_failure_with_errors() {
        let r = parse_receipt(FAILURE).unwrap();
        assert!(!r.success);
        assert!(r.accessions.is_empty());
        assert_eq!(r.errors.len(), 2);
        assert!(r.error_summary().contains("ERC000050"));
        assert!(r.error_summary().contains("Sample accession is missing"));
    }

    #[test]
    fn error_summary_falls_back_without_messages() {
        let r = parse_receipt(r#"<RECEIPT success="false"/>"#).unwrap();
        assert!(!r.success);
        assert!(r.error_summary().contains("not successful"));
    }

    #[test]
    fn success_flag_is_case_insensitive() {
        assert!(parse_receipt(r#"<RECEIPT success="TRUE"/>"#).unwrap().success);
        assert!(!parse_receipt(r#"<RECEIPT success="False"/>"#).unwrap().success);
    }

    #[test]
    fn empty_message_element_does_not_leak_into_later_text() {
        // A self-closing <INFO/> must not capture the text of a later, unrelated element.
        let xml = r#"<RECEIPT success="true"><MESSAGES><INFO/></MESSAGES><OTHER>stray text</OTHER></RECEIPT>"#;
        let r = parse_receipt(xml).unwrap();
        assert!(r.info.is_empty(), "info should be empty, got: {:?}", r.info);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn malformed_xml_is_error() {
        let err = parse_receipt("<RECEIPT success=").unwrap_err();
        assert!(err.contains("malformed receipt XML"), "got: {err}");
    }

    #[test]
    fn read_receipt_tags_path_on_bad_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("receipt.xml");
        std::fs::write(&path, "<RECEIPT success=").unwrap();
        let err = read_receipt(&path).unwrap_err().to_string();
        assert!(err.contains("receipt.xml"), "got: {err}");
    }
}
