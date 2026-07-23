//! Core domain types shared across the tool.
//!
//! The record structs ([`ReadRecord`], [`AssemblyRecord`], [`MagBin`]) model one row of their
//! respective input TSV. Strict field-level validation (controlled vocabularies, `ASSEMBLYNAME`
//! pattern, etc.) is layered on top in the `input` module (milestone 3); here the vocabulary-heavy
//! fields are kept as `String` because Webin-CLI is the authoritative validator for them.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The Webin-CLI submission context. Reads use `reads`; both plain assemblies and MAGs use `genome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Context {
    Reads,
    Genome,
}

impl Context {
    /// The literal passed to `webin-cli -context`.
    pub fn as_str(self) -> &'static str {
        match self {
            Context::Reads => "reads",
            Context::Genome => "genome",
        }
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether to validate only, or validate-and-submit. Maps to Webin-CLI `-validate` / `-submit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubmitMode {
    /// Validate the object without uploading. Safe, no accessions minted.
    #[default]
    Validate,
    /// Validate and submit for real.
    Submit,
}

impl SubmitMode {
    /// The Webin-CLI flag (without the leading `-`) this mode selects.
    pub fn flag(self) -> &'static str {
        match self {
            SubmitMode::Validate => "validate",
            SubmitMode::Submit => "submit",
        }
    }
}

/// Which ENA Webin service to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    /// The Webin **test** service — schema-identical to production but throwaway. The default.
    #[default]
    Test,
    /// The real production Webin service. Mints permanent accessions.
    Production,
}

impl Environment {
    /// Whether Webin-CLI should be invoked with `-test`.
    pub fn is_test(self) -> bool {
        matches!(self, Environment::Test)
    }
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Environment::Test => f.write_str("test"),
            Environment::Production => f.write_str("production"),
        }
    }
}

/// One row of the reads input: a single run/experiment to submit under `-context reads`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadRecord {
    /// Unique experiment name (Webin-CLI `NAME`), also used to name the manifest/output dir.
    pub name: String,
    /// Study accession or alias (`STUDY`). User-created.
    pub study: String,
    /// Sample accession or alias (`SAMPLE`). User-created.
    pub sample: String,
    pub platform: String,
    pub instrument: String,
    pub library_source: String,
    pub library_selection: String,
    pub library_strategy: String,
    #[serde(default)]
    pub library_name: Option<String>,
    #[serde(default)]
    pub insert_size: Option<u64>,
    #[serde(default)]
    pub description: Option<String>,
    /// Read data files. One or two FASTQ for single/paired, or a single BAM/CRAM.
    pub files: Vec<ReadFile>,
}

/// A read data file plus its Webin-CLI file-type keyword.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFile {
    pub kind: ReadFileKind,
    pub path: PathBuf,
}

/// Webin-CLI read file-type keywords.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ReadFileKind {
    Fastq,
    Bam,
    Cram,
}

impl ReadFileKind {
    pub fn manifest_key(self) -> &'static str {
        match self {
            ReadFileKind::Fastq => "FASTQ",
            ReadFileKind::Bam => "BAM",
            ReadFileKind::Cram => "CRAM",
        }
    }
}

/// One row of the assembly input: a single genome assembly under `-context genome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssemblyRecord {
    /// `ASSEMBLYNAME`: <= 50 chars, pattern `^[A-Za-z0-9][A-Za-z0-9 _#-.]*$`.
    pub assemblyname: String,
    pub study: String,
    pub sample: String,
    /// `ASSEMBLY_TYPE`, e.g. "clone or isolate" or "Metagenome-Assembled Genome (MAG)".
    pub assembly_type: String,
    pub coverage: String,
    pub program: String,
    pub platform: String,
    #[serde(default)]
    pub moleculetype: Option<String>,
    #[serde(default)]
    pub mingaplength: Option<u64>,
    #[serde(default)]
    pub description: Option<String>,
    /// Comma-joined run accessions for `RUN_REF`.
    #[serde(default)]
    pub run_ref: Option<String>,
    /// Sequence files keyed by Webin-CLI file type (FASTA, FLATFILE, AGP, ...).
    pub files: Vec<AssemblyFile>,
}

/// A genome sequence file plus its Webin-CLI file-type keyword.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssemblyFile {
    /// Webin-CLI keyword: FASTA, FLATFILE, AGP, CHROMOSOME_LIST, UNLOCALISED_LIST.
    pub kind: String,
    pub path: PathBuf,
}

/// An accession returned by ENA in a submission receipt (milestone 7) and recorded in the local
/// history: the accessioned object's kind (e.g. `ANALYSIS`, `RUN`, `EXPERIMENT`) and its identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accession {
    /// The kind of object accessioned, as named in the receipt element (e.g. `ANALYSIS`, `RUN`).
    #[serde(rename = "type")]
    pub kind: String,
    /// The accession identifier itself (e.g. `ERZ1234567`).
    pub accession: String,
}

/// The `ASSEMBLY_TYPE` value that marks a genome assembly as a MAG.
///
/// MAG assemblies are submitted with [`AssemblyRecord`] under `-context genome`, with this value in
/// `assembly_type` and `sample` set to the derived MAG sample accession. The MAG **sample sheet**
/// itself is supplied by the user and parsed generically (see `input::Table`) so all its checklist
/// columns are preserved while the tool fills only `tax_id`.
pub const MAG_ASSEMBLY_TYPE: &str = "Metagenome-Assembled Genome (MAG)";
