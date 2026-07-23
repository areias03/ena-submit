//! TSV input parsing and validation.
//!
//! Two layers:
//! - [`Table`] is a generic, order-preserving tab-separated reader used for the user-supplied MAG
//!   **sample sheet** (where every checklist column must be preserved while only `tax_id` is filled)
//!   and the `bin_name -> sample accession` mapping.
//! - [`read_reads`] / [`read_assemblies`] map a `Table` onto the strongly-typed [`ReadRecord`] /
//!   [`AssemblyRecord`] domain structs, collecting *all* row-level problems into a single error.
//!
//! Parsing is deliberately plain: fields are split on `\t` with no quote processing, matching how
//! ENA/pipeline TSVs are emitted. Header matching is case-insensitive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::chromosome::Topology;
use crate::error::{Error, Result};
use crate::model::{AssemblyFile, AssemblyRecord, MagBin, ReadFile, ReadFileKind, ReadRecord};

/// A parsed tab-separated table: header row plus data rows, order preserved.
#[derive(Debug, Clone)]
pub struct Table {
    /// Source path, used for error messages.
    pub path: PathBuf,
    /// Header cells exactly as written (trimmed).
    pub headers: Vec<String>,
    /// Data rows, each guaranteed to have `headers.len()` cells.
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// Read and parse a TSV file.
    pub fn read(path: &Path) -> Result<Table> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Table::parse(path.to_path_buf(), &text)
    }

    /// Parse TSV text. Blank lines are skipped. Every data row must have the same number of cells
    /// as the header row, and at least one data row is required — a header-only file is almost
    /// always an unfilled template, and silently "succeeding" with nothing to do hides that.
    pub fn parse(path: PathBuf, text: &str) -> Result<Table> {
        let mut lines = text
            .lines()
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty());

        let (_, header_line) = lines.next().ok_or_else(|| Error::Input {
            path: path.clone(),
            message: "file is empty (no header row)".to_string(),
        })?;
        let headers: Vec<String> = header_line
            .split('\t')
            .map(|c| c.trim().to_string())
            .collect();

        let mut rows = Vec::new();
        let mut problems = Vec::new();
        for (line_no, line) in lines {
            let cells: Vec<String> = line.split('\t').map(|c| c.trim().to_string()).collect();
            if cells.len() != headers.len() {
                // line_no is 0-based over all lines; +1 makes it the file line number.
                problems.push(format!(
                    "line {}: expected {} columns, found {}",
                    line_no + 1,
                    headers.len(),
                    cells.len()
                ));
                continue;
            }
            rows.push(cells);
        }

        if !problems.is_empty() {
            return Err(Error::Input {
                path,
                message: problems.join("\n"),
            });
        }
        if rows.is_empty() {
            return Err(Error::Input {
                path,
                message: "no data rows: the file has a header but nothing to process".to_string(),
            });
        }
        Ok(Table {
            path,
            headers,
            rows,
        })
    }

    /// Case-insensitive header -> column-index map.
    fn header_index(&self) -> HashMap<String, usize> {
        self.headers
            .iter()
            .enumerate()
            .map(|(i, h)| (h.to_ascii_lowercase(), i))
            .collect()
    }

    /// Column index for `name` (case-insensitive), if the column exists.
    pub fn column(&self, name: &str) -> Option<usize> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .position(|h| h.eq_ignore_ascii_case(&name))
    }
}

/// A view over one data row, indexed by header name, that accumulates problems.
struct Row<'a> {
    index: &'a HashMap<String, usize>,
    cells: &'a [String],
    /// 1-based data-row number for messages.
    number: usize,
    problems: &'a mut Vec<String>,
}

impl Row<'_> {
    /// Trimmed non-empty value for `col`, or `None` if the column is absent or the cell is blank.
    fn opt(&self, col: &str) -> Option<&str> {
        self.index
            .get(col)
            .and_then(|&i| self.cells.get(i))
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
    }

    /// Like [`opt`], but records a "missing required" problem and returns `None` when absent.
    fn required(&mut self, col: &str) -> Option<String> {
        match self.opt(col) {
            Some(v) => Some(v.to_string()),
            None => {
                self.problem(format!("missing required '{col}'"));
                None
            }
        }
    }

    /// Parse an optional `u64` cell, recording a problem on malformed input.
    fn opt_u64(&mut self, col: &str) -> Option<u64> {
        match self.opt(col) {
            None => None,
            Some(v) => match v.parse::<u64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    self.problem(format!("'{col}' = '{v}' is not a non-negative integer"));
                    None
                }
            },
        }
    }

    fn problem(&mut self, msg: impl Into<String>) {
        self.problems
            .push(format!("row {}: {}", self.number, msg.into()));
    }
}

/// Mandatory columns for a reads TSV.
const READS_REQUIRED: &[&str] = &[
    "name",
    "study",
    "sample",
    "platform",
    "instrument",
    "library_source",
    "library_selection",
    "library_strategy",
];

/// Read data file columns and the [`ReadFileKind`] each maps to.
const READ_FILE_COLUMNS: &[(&str, ReadFileKind)] = &[
    ("fastq1", ReadFileKind::Fastq),
    ("fastq2", ReadFileKind::Fastq),
    ("bam", ReadFileKind::Bam),
    ("cram", ReadFileKind::Cram),
];

/// Parse a reads TSV into validated [`ReadRecord`]s.
pub fn read_reads(path: &Path) -> Result<Vec<ReadRecord>> {
    reads_from_table(&Table::read(path)?)
}

fn reads_from_table(table: &Table) -> Result<Vec<ReadRecord>> {
    let index = table.header_index();
    ensure_columns(table, READS_REQUIRED)?;
    if !READ_FILE_COLUMNS
        .iter()
        .any(|(c, _)| index.contains_key(*c))
    {
        return Err(Error::Input {
            path: table.path.clone(),
            message: "no read-file column found: provide at least one of fastq1, bam, cram"
                .to_string(),
        });
    }

    let mut problems = Vec::new();
    let mut out = Vec::new();
    for (i, cells) in table.rows.iter().enumerate() {
        let mut row = Row {
            index: &index,
            cells,
            number: i + 1,
            problems: &mut problems,
        };

        let name = row.required("name");
        let study = row.required("study");
        let sample = row.required("sample");
        let platform = row.required("platform");
        let instrument = row.required("instrument");
        let library_source = row.required("library_source");
        let library_selection = row.required("library_selection");
        let library_strategy = row.required("library_strategy");
        let insert_size = row.opt_u64("insert_size");
        let library_name = row.opt("library_name").map(str::to_string);
        let description = row.opt("description").map(str::to_string);

        let files = read_files(&mut row);

        // Only assemble the record if every fallible piece succeeded for this row.
        if let (
            Some(name),
            Some(study),
            Some(sample),
            Some(platform),
            Some(instrument),
            Some(library_source),
            Some(library_selection),
            Some(library_strategy),
            Some(files),
        ) = (
            name,
            study,
            sample,
            platform,
            instrument,
            library_source,
            library_selection,
            library_strategy,
            files,
        ) {
            out.push(ReadRecord {
                name,
                study,
                sample,
                platform,
                instrument,
                library_source,
                library_selection,
                library_strategy,
                library_name,
                insert_size,
                description,
                files,
            });
        }
    }

    finish(table, problems, out)
}

/// Collect and validate the read-file set for one row. Enforces: at least one file; a single file
/// kind (no mixing FASTQ with BAM/CRAM); and `fastq2` only alongside `fastq1`.
fn read_files(row: &mut Row) -> Option<Vec<ReadFile>> {
    let mut files = Vec::new();
    let mut kinds = Vec::new();
    for (col, kind) in READ_FILE_COLUMNS {
        if let Some(path) = row.opt(col) {
            files.push(ReadFile {
                kind: *kind,
                path: path.into(),
            });
            kinds.push(*kind);
        }
    }

    if files.is_empty() {
        row.problem("no read file given (need fastq1[/fastq2], bam, or cram)");
        return None;
    }
    if row.opt("fastq2").is_some() && row.opt("fastq1").is_none() {
        row.problem("fastq2 given without fastq1");
        return None;
    }
    if kinds.iter().any(|k| *k != kinds[0]) {
        row.problem("mixed read file types; use only one of FASTQ, BAM, or CRAM");
        return None;
    }
    Some(files)
}

/// Mandatory columns for an assemblies TSV.
const ASSEMBLIES_REQUIRED: &[&str] = &[
    "assemblyname",
    "study",
    "sample",
    "assembly_type",
    "coverage",
    "program",
    "platform",
];

/// Assembly sequence-file columns mapped to their Webin-CLI manifest keyword.
const ASSEMBLY_FILE_COLUMNS: &[(&str, &str)] = &[
    ("fasta", "FASTA"),
    ("flatfile", "FLATFILE"),
    ("agp", "AGP"),
    ("chromosome_list", "CHROMOSOME_LIST"),
    ("unlocalised_list", "UNLOCALISED_LIST"),
];

/// Parse an assemblies TSV into validated [`AssemblyRecord`]s.
pub fn read_assemblies(path: &Path) -> Result<Vec<AssemblyRecord>> {
    assemblies_from_table(&Table::read(path)?)
}

fn assemblies_from_table(table: &Table) -> Result<Vec<AssemblyRecord>> {
    let index = table.header_index();
    ensure_columns(table, ASSEMBLIES_REQUIRED)?;
    if !ASSEMBLY_FILE_COLUMNS
        .iter()
        .any(|(c, _)| index.contains_key(*c))
    {
        return Err(Error::Input {
            path: table.path.clone(),
            message: "no sequence-file column found: provide at least fasta or flatfile"
                .to_string(),
        });
    }

    let mut problems = Vec::new();
    let mut out = Vec::new();
    for (i, cells) in table.rows.iter().enumerate() {
        let mut row = Row {
            index: &index,
            cells,
            number: i + 1,
            problems: &mut problems,
        };

        let assemblyname = row
            .required("assemblyname")
            .filter(|n| valid_assemblyname(n, &mut row));
        let study = row.required("study");
        let sample = row.required("sample");
        let assembly_type = row.required("assembly_type");
        let coverage = row.required("coverage");
        let program = row.required("program");
        let platform = row.required("platform");
        let moleculetype = row.opt("moleculetype").map(str::to_string);
        let mingaplength = row.opt_u64("mingaplength");
        let description = row.opt("description").map(str::to_string);
        let run_ref = row.opt("run_ref").map(str::to_string);
        let files = assembly_files(&mut row);

        if let (
            Some(assemblyname),
            Some(study),
            Some(sample),
            Some(assembly_type),
            Some(coverage),
            Some(program),
            Some(platform),
            Some(files),
        ) = (
            assemblyname,
            study,
            sample,
            assembly_type,
            coverage,
            program,
            platform,
            files,
        ) {
            out.push(AssemblyRecord {
                assemblyname,
                study,
                sample,
                assembly_type,
                coverage,
                program,
                platform,
                moleculetype,
                mingaplength,
                description,
                run_ref,
                files,
            });
        }
    }

    finish(table, problems, out)
}

fn assembly_files(row: &mut Row) -> Option<Vec<AssemblyFile>> {
    let mut files = Vec::new();
    for (col, keyword) in ASSEMBLY_FILE_COLUMNS {
        if let Some(path) = row.opt(col) {
            files.push(AssemblyFile {
                kind: (*keyword).to_string(),
                path: path.into(),
            });
        }
    }
    let has_sequence = files
        .iter()
        .any(|f| f.kind == "FASTA" || f.kind == "FLATFILE");
    if !has_sequence {
        row.problem("no sequence file (need fasta or flatfile)");
        return None;
    }
    Some(files)
}

/// ENA `ASSEMBLYNAME` rule: <= 50 chars, pattern `^[A-Za-z0-9][A-Za-z0-9 _#-.]*$`.
fn valid_assemblyname(name: &str, row: &mut Row) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9 _#\-.]*$").unwrap());
    if name.chars().count() > 50 {
        row.problem(format!("assemblyname '{name}' exceeds 50 characters"));
        return false;
    }
    if !re.is_match(name) {
        row.problem(format!(
            "assemblyname '{name}' has invalid characters (allowed: letters, digits, space, _ # - .)"
        ));
        return false;
    }
    true
}

/// Mandatory columns for a MAG-assembly TSV. `sample`/`assembly_type` are supplied by the tool
/// (from the registered-samples mapping and the fixed MAG value), so they are not user columns.
const MAG_ASSEMBLIES_REQUIRED: &[&str] = &[
    "bin_name",
    "assemblyname",
    "study",
    "coverage",
    "program",
    "platform",
    "fasta",
];

/// Parse a MAG-assembly TSV into validated [`MagBin`]s (one per bin).
pub fn read_mag_assemblies(path: &Path) -> Result<Vec<MagBin>> {
    mag_assemblies_from_table(&Table::read(path)?)
}

fn mag_assemblies_from_table(table: &Table) -> Result<Vec<MagBin>> {
    let index = table.header_index();
    ensure_columns(table, MAG_ASSEMBLIES_REQUIRED)?;

    let mut problems = Vec::new();
    let mut out = Vec::new();
    for (i, cells) in table.rows.iter().enumerate() {
        let mut row = Row {
            index: &index,
            cells,
            number: i + 1,
            problems: &mut problems,
        };

        let bin_name = row.required("bin_name");
        let assemblyname = row
            .required("assemblyname")
            .filter(|n| valid_assemblyname(n, &mut row));
        let study = row.required("study");
        let coverage = row.required("coverage");
        let program = row.required("program");
        let platform = row.required("platform");
        let fasta = row.required("fasta");
        let run_ref = row.opt("run_ref").map(str::to_string);
        let description = row.opt("description").map(str::to_string);
        let chromosome_name = row.opt("chromosome_name").map(str::to_string);
        let topology = match row.opt("topology") {
            None => Some(Topology::default()),
            Some(v) => match Topology::parse(v) {
                Some(t) => Some(t),
                None => {
                    row.problem(format!("topology '{v}' is not 'linear' or 'circular'"));
                    None
                }
            },
        };

        if let (
            Some(bin_name),
            Some(assemblyname),
            Some(study),
            Some(coverage),
            Some(program),
            Some(platform),
            Some(fasta),
            Some(topology),
        ) = (
            bin_name,
            assemblyname,
            study,
            coverage,
            program,
            platform,
            fasta,
            topology,
        ) {
            out.push(MagBin {
                bin_name,
                assemblyname,
                study,
                coverage,
                program,
                platform,
                fasta: fasta.into(),
                run_ref,
                description,
                topology,
                chromosome_name,
            });
        }
    }

    finish(table, problems, out)
}

/// Read a `bin_name -> sample_accession` mapping TSV (used to submit MAG assemblies).
pub fn read_sample_map(path: &Path) -> Result<HashMap<String, String>> {
    let table = Table::read(path)?;
    ensure_columns(&table, &["bin_name", "sample_accession"])?;
    let index = table.header_index();

    let mut problems = Vec::new();
    let mut map = HashMap::new();
    for (i, cells) in table.rows.iter().enumerate() {
        let mut row = Row {
            index: &index,
            cells,
            number: i + 1,
            problems: &mut problems,
        };
        let bin = row.required("bin_name");
        let acc = row.required("sample_accession");
        if let (Some(bin), Some(acc)) = (bin, acc) {
            if map.insert(bin.clone(), acc).is_some() {
                row.problem(format!("duplicate bin_name '{bin}'"));
            }
        }
    }
    finish(&table, problems, map)
}

/// Structural check that every required column header exists.
fn ensure_columns(table: &Table, required: &[&str]) -> Result<()> {
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|c| table.column(c).is_none())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::Input {
            path: table.path.clone(),
            message: format!("missing required column(s): {}", missing.join(", ")),
        })
    }
}

/// Return `value` if no problems were collected, otherwise an aggregated [`Error::Input`].
fn finish<T>(table: &Table, problems: Vec<String>, value: T) -> Result<T> {
    if problems.is_empty() {
        Ok(value)
    } else {
        Err(Error::Input {
            path: table.path.clone(),
            message: problems.join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Table {
        Table::parse(PathBuf::from("test.tsv"), text).unwrap()
    }

    #[test]
    fn parses_generic_table_preserving_columns() {
        let t = table("a\tb\tc\n1\t2\t3\n4\t5\t6\n");
        assert_eq!(t.headers, ["a", "b", "c"]);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1], ["4", "5", "6"]);
        assert_eq!(t.column("B"), Some(1)); // case-insensitive
        assert_eq!(t.column("missing"), None);
    }

    #[test]
    fn header_only_table_is_an_error() {
        // An unfilled template must not be reported as a successful no-op run.
        let err = Table::parse(PathBuf::from("t.tsv"), "a\tb\n").unwrap_err();
        assert!(err.to_string().contains("no data rows"), "got: {err}");

        // Trailing blank lines do not count as data.
        let err = Table::parse(PathBuf::from("t.tsv"), "a\tb\n\n  \n").unwrap_err();
        assert!(err.to_string().contains("no data rows"), "got: {err}");
    }

    #[test]
    fn ragged_row_is_an_error() {
        let err = Table::parse(PathBuf::from("t.tsv"), "a\tb\n1\n").unwrap_err();
        assert!(err.to_string().contains("expected 2 columns, found 1"));
    }

    const READS_HEADER: &str = "name\tstudy\tsample\tplatform\tinstrument\tlibrary_source\tlibrary_selection\tlibrary_strategy\tfastq1\tfastq2";

    #[test]
    fn reads_paired_fastq_ok() {
        let t = table(&format!(
            "{READS_HEADER}\nrun1\tPRJEB1\tERS1\tILLUMINA\tNovaSeq\tGENOMIC\tRANDOM\tWGS\tr1.fq.gz\tr2.fq.gz\n"
        ));
        let recs = reads_from_table(&t).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].files.len(), 2);
        assert_eq!(recs[0].files[0].kind, ReadFileKind::Fastq);
    }

    #[test]
    fn reads_missing_required_column_is_structural_error() {
        // Drop the `study` column entirely.
        let t = table(
            "name\tsample\tplatform\tinstrument\tlibrary_source\tlibrary_selection\tlibrary_strategy\tfastq1\nrun1\tERS1\tILLUMINA\tNovaSeq\tGENOMIC\tRANDOM\tWGS\tr1.fq.gz\n",
        );
        let err = reads_from_table(&t).unwrap_err();
        assert!(err.to_string().contains("missing required column"));
        assert!(err.to_string().contains("study"));
    }

    #[test]
    fn reads_missing_required_value_reports_row() {
        let t = table(&format!(
            "{READS_HEADER}\n\tPRJEB1\tERS1\tILLUMINA\tNovaSeq\tGENOMIC\tRANDOM\tWGS\tr1.fq.gz\t\n"
        ));
        let err = reads_from_table(&t).unwrap_err();
        assert!(err.to_string().contains("row 1"));
        assert!(err.to_string().contains("missing required 'name'"));
    }

    #[test]
    fn reads_fastq2_without_fastq1_rejected() {
        let t = table(&format!(
            "{READS_HEADER}\nrun1\tPRJEB1\tERS1\tILLUMINA\tNovaSeq\tGENOMIC\tRANDOM\tWGS\t\tr2.fq.gz\n"
        ));
        let err = reads_from_table(&t).unwrap_err();
        assert!(err.to_string().contains("fastq2 given without fastq1"));
    }

    #[test]
    fn reads_no_file_column_is_structural_error() {
        let t = table(
            "name\tstudy\tsample\tplatform\tinstrument\tlibrary_source\tlibrary_selection\tlibrary_strategy\nrun1\tPRJEB1\tERS1\tILLUMINA\tNovaSeq\tGENOMIC\tRANDOM\tWGS\n",
        );
        let err = reads_from_table(&t).unwrap_err();
        assert!(err.to_string().contains("no read-file column"));
    }

    const ASM_HEADER: &str =
        "assemblyname\tstudy\tsample\tassembly_type\tcoverage\tprogram\tplatform\tfasta";

    #[test]
    fn assemblies_ok() {
        let t = table(&format!(
            "{ASM_HEADER}\nasm1\tPRJEB1\tERS1\tclone or isolate\t30\tSPAdes\tILLUMINA\tasm.fa.gz\n"
        ));
        let recs = read_from(&t);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].files[0].kind, "FASTA");
    }

    fn read_from(t: &Table) -> Vec<AssemblyRecord> {
        assemblies_from_table(t).unwrap()
    }

    #[test]
    fn assemblyname_too_long_rejected() {
        let long = "a".repeat(51);
        let t = table(&format!(
            "{ASM_HEADER}\n{long}\tPRJEB1\tERS1\tclone or isolate\t30\tSPAdes\tILLUMINA\tasm.fa.gz\n"
        ));
        let err = assemblies_from_table(&t).unwrap_err();
        assert!(err.to_string().contains("exceeds 50 characters"));
    }

    #[test]
    fn assemblyname_bad_chars_rejected() {
        let t = table(&format!(
            "{ASM_HEADER}\nbad/name\tPRJEB1\tERS1\tclone or isolate\t30\tSPAdes\tILLUMINA\tasm.fa.gz\n"
        ));
        let err = assemblies_from_table(&t).unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn assemblyname_with_allowed_punctuation_ok() {
        let t = table(&format!(
            "{ASM_HEADER}\nasm_1 #2-final.v3\tPRJEB1\tERS1\tclone or isolate\t30\tSPAdes\tILLUMINA\tasm.fa.gz\n"
        ));
        assert_eq!(assemblies_from_table(&t).unwrap().len(), 1);
    }

    #[test]
    fn assembly_requires_sequence_file() {
        // Only an AGP column present, no fasta/flatfile.
        let t = table(
            "assemblyname\tstudy\tsample\tassembly_type\tcoverage\tprogram\tplatform\tagp\nasm1\tPRJEB1\tERS1\tclone or isolate\t30\tSPAdes\tILLUMINA\tscaffolds.agp\n",
        );
        let err = assemblies_from_table(&t).unwrap_err();
        assert!(err.to_string().contains("no sequence file"));
    }

    #[test]
    fn multiple_row_errors_are_aggregated() {
        let t = table(&format!(
            "{READS_HEADER}\n\tPRJEB1\tERS1\tILLUMINA\tNovaSeq\tGENOMIC\tRANDOM\tWGS\tr1.fq.gz\t\nrun2\t\tERS1\tILLUMINA\tNovaSeq\tGENOMIC\tRANDOM\tWGS\tr1.fq.gz\t\n"
        ));
        let err = reads_from_table(&t).unwrap_err().to_string();
        assert!(err.contains("row 1"), "got: {err}");
        assert!(err.contains("row 2"), "got: {err}");
    }

    const MAG_HEADER: &str =
        "bin_name\tassemblyname\tstudy\tcoverage\tprogram\tplatform\tfasta\ttopology";

    #[test]
    fn mag_assemblies_ok_with_topology() {
        let t = table(&format!(
            "{MAG_HEADER}\nbin.1\tMAG_bin.1\tPRJEB1\t25\tmetaSPAdes\tILLUMINA\tbins/bin.1.fa.gz\tcircular\n"
        ));
        let bins = mag_assemblies_from_table(&t).unwrap();
        assert_eq!(bins.len(), 1);
        assert_eq!(bins[0].bin_name, "bin.1");
        assert_eq!(bins[0].fasta, PathBuf::from("bins/bin.1.fa.gz"));
        assert_eq!(bins[0].topology, Topology::Circular);
        assert_eq!(bins[0].chromosome_name, None);
    }

    #[test]
    fn mag_assemblies_default_topology_is_linear() {
        // No topology column at all.
        let t = table(
            "bin_name\tassemblyname\tstudy\tcoverage\tprogram\tplatform\tfasta\nbin.1\tMAG_bin.1\tPRJEB1\t25\tmetaSPAdes\tILLUMINA\tb.fa.gz\n",
        );
        let bins = mag_assemblies_from_table(&t).unwrap();
        assert_eq!(bins[0].topology, Topology::Linear);
    }

    #[test]
    fn mag_assemblies_bad_topology_rejected() {
        let t = table(&format!(
            "{MAG_HEADER}\nbin.1\tMAG_bin.1\tPRJEB1\t25\tmetaSPAdes\tILLUMINA\tb.fa.gz\tsupercoiled\n"
        ));
        let err = mag_assemblies_from_table(&t).unwrap_err().to_string();
        assert!(err.contains("topology"), "got: {err}");
    }

    #[test]
    fn mag_assemblies_missing_fasta_column_is_structural_error() {
        let t = table(
            "bin_name\tassemblyname\tstudy\tcoverage\tprogram\tplatform\nbin.1\tMAG_bin.1\tPRJEB1\t25\tmetaSPAdes\tILLUMINA\n",
        );
        let err = mag_assemblies_from_table(&t).unwrap_err().to_string();
        assert!(err.contains("missing required column"), "got: {err}");
        assert!(err.contains("fasta"), "got: {err}");
    }

    #[test]
    fn sample_map_parses() {
        let t = table("bin_name\tsample_accession\nbin.1\tERS100\nbin.2\tERS101\n");
        // Exercise via the table-backed path.
        ensure_columns(&t, &["bin_name", "sample_accession"]).unwrap();
        assert_eq!(t.rows.len(), 2);
    }
}
