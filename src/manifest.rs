//! Render Webin-CLI manifest files.
//!
//! A manifest is a plain-text file, one per submitted object, of `KEY<TAB>value` lines: the object
//! metadata followed by one line per data file (`FASTQ`, `FASTA`, ...). These functions are the
//! pure rendering step — turning a validated [`ReadRecord`] / [`AssemblyRecord`] into manifest
//! text. Writing the file and invoking Webin-CLI is the submission layer's job (a later milestone).
//!
//! Field keys and their mandatory/optional split follow the ENA Webin-CLI documentation for the
//! `reads` and `genome` contexts. Mandatory fields are always emitted (the `input` layer guarantees
//! they are present); optional fields are emitted only when set.

use crate::model::{AssemblyRecord, ReadRecord};

/// Accumulates `KEY<TAB>value` manifest lines. Optional fields are skipped when absent, so the
/// rendered manifest contains only keys the user actually provided.
struct ManifestBuilder {
    lines: Vec<String>,
}

impl ManifestBuilder {
    fn new() -> Self {
        ManifestBuilder { lines: Vec::new() }
    }

    /// Emit `KEY<TAB>value`.
    fn field(&mut self, key: &str, value: &str) -> &mut Self {
        self.lines.push(format!("{key}\t{value}"));
        self
    }

    /// Emit `KEY<TAB>value` only when `value` is present.
    fn opt(&mut self, key: &str, value: Option<&str>) -> &mut Self {
        if let Some(v) = value {
            self.field(key, v);
        }
        self
    }

    /// Emit a numeric field only when present.
    fn opt_num(&mut self, key: &str, value: Option<u64>) -> &mut Self {
        if let Some(v) = value {
            self.field(key, &v.to_string());
        }
        self
    }

    /// Join the lines into a manifest string with a trailing newline.
    fn finish(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }
}

/// Render a Webin-CLI `reads` manifest for one run/experiment.
pub fn reads_manifest(record: &ReadRecord) -> String {
    let mut m = ManifestBuilder::new();
    m.field("STUDY", &record.study)
        .field("SAMPLE", &record.sample)
        .field("NAME", &record.name)
        .field("PLATFORM", &record.platform)
        .field("INSTRUMENT", &record.instrument)
        .field("LIBRARY_SOURCE", &record.library_source)
        .field("LIBRARY_SELECTION", &record.library_selection)
        .field("LIBRARY_STRATEGY", &record.library_strategy)
        .opt("LIBRARY_NAME", record.library_name.as_deref())
        .opt_num("INSERT_SIZE", record.insert_size)
        .opt("DESCRIPTION", record.description.as_deref());
    for file in &record.files {
        m.field(file.kind.manifest_key(), &file.path.to_string_lossy());
    }
    m.finish()
}

/// Render a Webin-CLI `genome` manifest for one assembly (plain or MAG).
pub fn genome_manifest(record: &AssemblyRecord) -> String {
    let mut m = ManifestBuilder::new();
    m.field("STUDY", &record.study)
        .field("SAMPLE", &record.sample)
        .field("ASSEMBLYNAME", &record.assemblyname)
        .field("ASSEMBLY_TYPE", &record.assembly_type)
        .field("COVERAGE", &record.coverage)
        .field("PROGRAM", &record.program)
        .field("PLATFORM", &record.platform)
        .opt("MOLECULETYPE", record.moleculetype.as_deref())
        .opt_num("MINGAPLENGTH", record.mingaplength)
        .opt("DESCRIPTION", record.description.as_deref())
        .opt("RUN_REF", record.run_ref.as_deref());
    for file in &record.files {
        m.field(&file.kind, &file.path.to_string_lossy());
    }
    m.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AssemblyFile, ReadFile, ReadFileKind};
    use std::path::PathBuf;

    /// Parse a rendered manifest back into `(key, value)` pairs for order-independent assertions.
    fn fields(manifest: &str) -> Vec<(String, String)> {
        manifest
            .lines()
            .map(|l| {
                let (k, v) = l.split_once('\t').expect("each line is KEY<TAB>value");
                (k.to_string(), v.to_string())
            })
            .collect()
    }

    fn value<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
        fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn minimal_read() -> ReadRecord {
        ReadRecord {
            name: "run1".into(),
            study: "PRJEB1".into(),
            sample: "ERS1".into(),
            platform: "ILLUMINA".into(),
            instrument: "Illumina NovaSeq 6000".into(),
            library_source: "GENOMIC".into(),
            library_selection: "RANDOM".into(),
            library_strategy: "WGS".into(),
            library_name: None,
            insert_size: None,
            description: None,
            files: vec![ReadFile {
                kind: ReadFileKind::Fastq,
                path: PathBuf::from("reads_1.fastq.gz"),
            }],
        }
    }

    #[test]
    fn reads_manifest_has_mandatory_fields_and_file() {
        let f = fields(&reads_manifest(&minimal_read()));
        assert_eq!(value(&f, "STUDY"), Some("PRJEB1"));
        assert_eq!(value(&f, "SAMPLE"), Some("ERS1"));
        assert_eq!(value(&f, "NAME"), Some("run1"));
        assert_eq!(value(&f, "INSTRUMENT"), Some("Illumina NovaSeq 6000"));
        assert_eq!(value(&f, "LIBRARY_STRATEGY"), Some("WGS"));
        assert_eq!(value(&f, "FASTQ"), Some("reads_1.fastq.gz"));
    }

    #[test]
    fn reads_manifest_omits_absent_optionals() {
        let m = reads_manifest(&minimal_read());
        assert!(!m.contains("LIBRARY_NAME"));
        assert!(!m.contains("INSERT_SIZE"));
        assert!(!m.contains("DESCRIPTION"));
    }

    #[test]
    fn reads_manifest_includes_present_optionals_and_paired_files() {
        let mut r = minimal_read();
        r.library_name = Some("lib1".into());
        r.insert_size = Some(350);
        r.description = Some("Example paired-end run".into());
        r.files.push(ReadFile {
            kind: ReadFileKind::Fastq,
            path: PathBuf::from("reads_2.fastq.gz"),
        });

        let m = reads_manifest(&r);
        let f = fields(&m);
        assert_eq!(value(&f, "LIBRARY_NAME"), Some("lib1"));
        assert_eq!(value(&f, "INSERT_SIZE"), Some("350"));
        assert_eq!(value(&f, "DESCRIPTION"), Some("Example paired-end run"));
        // Both FASTQ files appear, once each.
        let fastqs: Vec<&str> = f
            .iter()
            .filter(|(k, _)| k == "FASTQ")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(fastqs, ["reads_1.fastq.gz", "reads_2.fastq.gz"]);
    }

    #[test]
    fn manifest_ends_with_single_trailing_newline() {
        let m = reads_manifest(&minimal_read());
        assert!(m.ends_with('\n'));
        assert!(!m.ends_with("\n\n"));
    }

    fn minimal_assembly() -> AssemblyRecord {
        AssemblyRecord {
            assemblyname: "asm1".into(),
            study: "PRJEB1".into(),
            sample: "ERS1".into(),
            assembly_type: "clone or isolate".into(),
            coverage: "30".into(),
            program: "SPAdes".into(),
            platform: "ILLUMINA".into(),
            moleculetype: None,
            mingaplength: None,
            description: None,
            run_ref: None,
            files: vec![AssemblyFile {
                kind: "FASTA".into(),
                path: PathBuf::from("assembly.fasta.gz"),
            }],
        }
    }

    #[test]
    fn genome_manifest_has_mandatory_fields_and_file() {
        let f = fields(&genome_manifest(&minimal_assembly()));
        assert_eq!(value(&f, "ASSEMBLYNAME"), Some("asm1"));
        assert_eq!(value(&f, "ASSEMBLY_TYPE"), Some("clone or isolate"));
        assert_eq!(value(&f, "COVERAGE"), Some("30"));
        assert_eq!(value(&f, "PROGRAM"), Some("SPAdes"));
        assert_eq!(value(&f, "PLATFORM"), Some("ILLUMINA"));
        assert_eq!(value(&f, "FASTA"), Some("assembly.fasta.gz"));
    }

    #[test]
    fn genome_manifest_omits_absent_optionals() {
        let m = genome_manifest(&minimal_assembly());
        assert!(!m.contains("MOLECULETYPE"));
        assert!(!m.contains("MINGAPLENGTH"));
        assert!(!m.contains("RUN_REF"));
        assert!(!m.contains("DESCRIPTION"));
    }

    #[test]
    fn genome_manifest_includes_present_optionals() {
        let mut a = minimal_assembly();
        a.moleculetype = Some("genomic DNA".into());
        a.mingaplength = Some(100);
        a.run_ref = Some("ERR0000000".into());
        a.description = Some("Example isolate assembly".into());

        let f = fields(&genome_manifest(&a));
        assert_eq!(value(&f, "MOLECULETYPE"), Some("genomic DNA"));
        assert_eq!(value(&f, "MINGAPLENGTH"), Some("100"));
        assert_eq!(value(&f, "RUN_REF"), Some("ERR0000000"));
        assert_eq!(value(&f, "DESCRIPTION"), Some("Example isolate assembly"));
    }

    #[test]
    fn genome_manifest_renders_mag_assembly_type() {
        let mut a = minimal_assembly();
        a.assembly_type = crate::model::MAG_ASSEMBLY_TYPE.to_string();
        let f = fields(&genome_manifest(&a));
        assert_eq!(
            value(&f, "ASSEMBLY_TYPE"),
            Some("Metagenome-Assembled Genome (MAG)")
        );
    }

    #[test]
    fn genome_manifest_emits_multiple_file_types() {
        let mut a = minimal_assembly();
        a.files.push(AssemblyFile {
            kind: "AGP".into(),
            path: PathBuf::from("scaffolds.agp"),
        });
        let f = fields(&genome_manifest(&a));
        assert_eq!(value(&f, "FASTA"), Some("assembly.fasta.gz"));
        assert_eq!(value(&f, "AGP"), Some("scaffolds.agp"));
    }
}
