//! Single-contig MAG detection and ENA chromosome list file generation.
//!
//! Long-read MAG assemblies often yield a bin that is a single (frequently circular) contig — a
//! *complete* genome rather than a set of fragments. ENA wants such an assembly submitted with a
//! **chromosome list file** naming the sequence as a chromosome, alongside the FASTA, instead of as
//! anonymous contigs (see [ADR 0006](../../docs/adr/0006-single-contig-mag-chromosome.md)).
//!
//! This module (milestone 8) is the pure core the MAG submission path needs: counting the sequences
//! in a bin's FASTA (transparently gzip-aware) and rendering / writing the chromosome list file.
//! Wiring into `mag submit` lands with that command (milestone 7).

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

use crate::error::{Error, Result};

/// Default ENA chromosome-type controlled-vocabulary value for a single-contig genome.
pub const DEFAULT_CHROMOSOME_TYPE: &str = "Chromosome";

/// Molecule topology, written as a modifier on the chromosome type (e.g. `Circular-Chromosome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Topology {
    #[default]
    Linear,
    Circular,
}

impl Topology {
    /// The modifier keyword ENA expects prefixed to the chromosome type.
    fn keyword(self) -> &'static str {
        match self {
            Topology::Linear => "Linear",
            Topology::Circular => "Circular",
        }
    }

    /// Parse a user-supplied topology cell (case-insensitive); `None` if unrecognised.
    pub fn parse(s: &str) -> Option<Topology> {
        match s.trim().to_ascii_lowercase().as_str() {
            "linear" => Some(Topology::Linear),
            "circular" => Some(Topology::Circular),
            _ => None,
        }
    }
}

/// One row of a chromosome list file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromosomeEntry {
    /// Sequence name; must match the FASTA `>` header's first token.
    pub object_name: String,
    /// Chromosome name (becomes the `/chromosome` qualifier).
    pub chromosome_name: String,
    /// ENA chromosome-type value, e.g. `Chromosome` or `Plasmid`.
    pub chromosome_type: String,
    pub topology: Topology,
}

/// Render chromosome list file text: one `OBJECT_NAME<TAB>CHROMOSOME_NAME<TAB>Topology-Type` line
/// per entry. The file has no header row and must be gzipped before submission (see
/// [`write_chromosome_list_gz`]).
pub fn render_chromosome_list(entries: &[ChromosomeEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str(&format!(
            "{}\t{}\t{}-{}\n",
            e.object_name,
            e.chromosome_name,
            e.topology.keyword(),
            e.chromosome_type
        ));
    }
    out
}

/// Write chromosome list `content` gzip-compressed to `path` (ENA requires gzipped data files).
pub fn write_chromosome_list_gz(path: &Path, content: &str) -> Result<()> {
    let file = File::create(path).map_err(|e| Error::io(path, e))?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder
        .write_all(content.as_bytes())
        .map_err(|e| Error::io(path, e))?;
    encoder.finish().map_err(|e| Error::io(path, e))?;
    Ok(())
}

/// Names of every sequence in a FASTA file (the first whitespace-delimited token after `>`),
/// transparently decompressing gzip.
pub fn sequence_names(fasta: &Path) -> Result<Vec<String>> {
    let reader = open_maybe_gzip(fasta)?;
    let mut names = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(|e| Error::io(fasta, e))?;
        if let Some(rest) = line.strip_prefix('>') {
            names.push(rest.split_whitespace().next().unwrap_or("").to_string());
        }
    }
    Ok(names)
}

/// The sole sequence name if `fasta` has exactly one sequence, else `None` (empty or multi-contig).
pub fn single_contig_name(fasta: &Path) -> Result<Option<String>> {
    let mut names = sequence_names(fasta)?;
    if names.len() == 1 {
        Ok(names.pop())
    } else {
        Ok(None)
    }
}

/// If `fasta` is a single-contig bin, build the chromosome list entry to submit it as a chromosome;
/// otherwise `None` (the caller keeps the default contigs submission). `chromosome_name`,
/// `chromosome_type`, and `topology` come from the MAG assembly metadata.
pub fn single_contig_entry(
    fasta: &Path,
    chromosome_name: &str,
    chromosome_type: &str,
    topology: Topology,
) -> Result<Option<ChromosomeEntry>> {
    Ok(single_contig_name(fasta)?.map(|object_name| ChromosomeEntry {
        object_name,
        chromosome_name: chromosome_name.to_string(),
        chromosome_type: chromosome_type.to_string(),
        topology,
    }))
}

/// Open `path`, sniffing the gzip magic bytes to decide whether to decompress. Peeking via
/// `fill_buf` leaves the bytes for the chosen reader to consume.
fn open_maybe_gzip(path: &Path) -> Result<Box<dyn BufRead>> {
    let file = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut reader = BufReader::new(file);
    let is_gzip = reader
        .fill_buf()
        .map_err(|e| Error::io(path, e))?
        .starts_with(&[0x1f, 0x8b]);
    if is_gzip {
        Ok(Box::new(BufReader::new(GzDecoder::new(reader))))
    } else {
        Ok(Box::new(reader))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::path::PathBuf;

    fn entry(object: &str, name: &str, topology: Topology) -> ChromosomeEntry {
        ChromosomeEntry {
            object_name: object.into(),
            chromosome_name: name.into(),
            chromosome_type: DEFAULT_CHROMOSOME_TYPE.into(),
            topology,
        }
    }

    #[test]
    fn renders_linear_and_circular_lines() {
        let text = render_chromosome_list(&[
            entry("contig_1", "bin.1", Topology::Circular),
            entry("contig_2", "bin.2", Topology::Linear),
        ]);
        assert_eq!(
            text,
            "contig_1\tbin.1\tCircular-Chromosome\ncontig_2\tbin.2\tLinear-Chromosome\n"
        );
    }

    #[test]
    fn topology_parses_case_insensitively() {
        assert_eq!(Topology::parse("Circular"), Some(Topology::Circular));
        assert_eq!(Topology::parse("  linear "), Some(Topology::Linear));
        assert_eq!(Topology::parse("supercoiled"), None);
        assert_eq!(Topology::default(), Topology::Linear);
    }

    /// Write `content` to a temp file and return its path (kept alive by the returned dir).
    fn write_temp(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn counts_sequences_in_plain_fasta() {
        let dir = tempfile::tempdir().unwrap();
        let fasta = write_temp(
            dir.path(),
            "bin.fasta",
            b">contig_1 length=5000 circular=true\nACGT\n>contig_2\nTTTT\n",
        );
        let names = sequence_names(&fasta).unwrap();
        // The description after the first token is dropped.
        assert_eq!(names, ["contig_1", "contig_2"]);
    }

    #[test]
    fn reads_gzipped_fasta_transparently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin.fasta.gz");
        let mut enc = GzEncoder::new(File::create(&path).unwrap(), Compression::default());
        enc.write_all(b">only_contig\nACGTACGT\n").unwrap();
        enc.finish().unwrap();

        assert_eq!(
            single_contig_name(&path).unwrap(),
            Some("only_contig".to_string())
        );
    }

    #[test]
    fn single_contig_name_reflects_sequence_count() {
        let dir = tempfile::tempdir().unwrap();

        let one = write_temp(dir.path(), "one.fa", b">c1\nACGT\n");
        assert_eq!(single_contig_name(&one).unwrap(), Some("c1".to_string()));

        let two = write_temp(dir.path(), "two.fa", b">c1\nACGT\n>c2\nTTTT\n");
        assert_eq!(single_contig_name(&two).unwrap(), None);

        let none = write_temp(dir.path(), "empty.fa", b"no headers here\n");
        assert_eq!(single_contig_name(&none).unwrap(), None);
    }

    #[test]
    fn single_contig_entry_builds_only_for_one_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let single = write_temp(dir.path(), "single.fa", b">tig00000001\nACGT\n");
        let e = single_contig_entry(&single, "bin.1", "Chromosome", Topology::Circular)
            .unwrap()
            .expect("single contig yields an entry");
        assert_eq!(e.object_name, "tig00000001");
        assert_eq!(e.chromosome_name, "bin.1");
        assert_eq!(e.topology, Topology::Circular);

        let multi = write_temp(dir.path(), "multi.fa", b">a\nAC\n>b\nGT\n");
        assert!(
            single_contig_entry(&multi, "bin.2", "Chromosome", Topology::Linear)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn write_gz_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chromosome_list.txt.gz");
        let entries = [entry("contig_1", "bin.1", Topology::Circular)];
        let content = render_chromosome_list(&entries);
        write_chromosome_list_gz(&path, &content).unwrap();

        // Bytes on disk are gzip, and decode back to the original text.
        let raw = std::fs::read(&path).unwrap();
        assert_eq!(&raw[..2], &[0x1f, 0x8b]);
        let mut decoded = String::new();
        GzDecoder::new(&raw[..]).read_to_string(&mut decoded).unwrap();
        assert_eq!(decoded, content);
    }
}
