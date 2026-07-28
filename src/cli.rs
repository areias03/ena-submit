//! Command-line interface: argument parsing and subcommand dispatch.
//!
//! Dispatch is thin: `init` scaffolds a project, `mag prepare` fills the sample sheet, `status`
//! renders the history, and the submission commands (`reads`, `assembly`, `mag submit`) read and
//! validate their input, render manifests, and drive Webin-CLI (via [`crate::webin`]) one object at
//! a time, appending a history record and reporting a summary.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use crate::chromosome::{self, DEFAULT_CHROMOSOME_TYPE};
use crate::config::{CONFIG_FILE, CONFIG_TEMPLATE, Config};
use crate::error::{Error, Result};
use crate::history::{History, Outcome, Record};
use crate::manifest;
use crate::model::{
    AssemblyFile, AssemblyRecord, Context, Environment, MAG_ASSEMBLY_TYPE, MagBin, SubmitMode,
};
use crate::webin::{self, WebinRun};

/// Submit reads, genome assemblies, and MAGs to the European Nucleotide Archive.
#[derive(Debug, Parser)]
#[command(name = "ena-submit", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Scaffold a config file and template input TSVs in the current directory.
    Init,

    /// Submit sequencing reads (`webin-cli -context reads`).
    Reads(SubmitArgs),

    /// Submit genome assemblies (`webin-cli -context genome`).
    Assembly(SubmitArgs),

    /// MAG workflow: prepare the sample TSV, then submit assemblies.
    #[command(subcommand)]
    Mag(MagCommand),

    /// Show the local submission history.
    Status,
}

/// Flags shared by the `reads` and `assembly` submission commands.
#[derive(Debug, Args)]
struct SubmitArgs {
    /// Input TSV describing the objects to submit (one row each).
    input: PathBuf,

    /// Directory that the file paths in the input are relative to (Webin-CLI `-inputDir`).
    #[arg(long)]
    input_dir: Option<PathBuf>,

    #[command(flatten)]
    mode: ModeArgs,

    #[command(flatten)]
    env: EnvArgs,
}

#[derive(Debug, Subcommand)]
enum MagCommand {
    /// Fill the `tax_id` column of a near-complete MAG sample TSV using the ENA taxonomy API,
    /// resolving each row's `scientific_name`. All other columns are passed through unchanged.
    Prepare {
        /// Your MAG sample TSV (all columns filled except `tax_id`).
        input: PathBuf,
        /// Where to write the completed sample TSV (ready to upload via the Webin spreadsheet UI).
        #[arg(short, long, default_value = "mag_samples.filled.tsv")]
        output: PathBuf,
    },
    /// Submit MAG assemblies once their derived samples are registered.
    Submit {
        /// Input TSV describing the MAG bins (same file used for `prepare`).
        input: PathBuf,
        /// TSV mapping `bin_name` to its registered `ERS…` sample accession.
        #[arg(long)]
        samples: PathBuf,
        /// Directory that the FASTA paths in the input are relative to.
        #[arg(long)]
        input_dir: Option<PathBuf>,
        #[command(flatten)]
        mode: ModeArgs,
        #[command(flatten)]
        env: EnvArgs,
    },
}

/// Mutually exclusive `--validate` / `--submit`. Defaults to validate-only for safety.
#[derive(Debug, Args)]
#[group(multiple = false)]
struct ModeArgs {
    /// Validate only, do not upload (default).
    #[arg(long)]
    validate: bool,
    /// Validate and submit for real.
    #[arg(long)]
    submit: bool,
}

impl ModeArgs {
    fn resolve(&self) -> SubmitMode {
        if self.submit {
            SubmitMode::Submit
        } else {
            SubmitMode::Validate
        }
    }
}

/// Mutually exclusive `--test` / `--production`. Falls back to the config default.
#[derive(Debug, Args)]
#[group(multiple = false)]
struct EnvArgs {
    /// Target the Webin test service.
    #[arg(long)]
    test: bool,
    /// Target the production Webin service (mints permanent accessions).
    #[arg(long)]
    production: bool,
}

impl EnvArgs {
    fn resolve(&self, default: Environment) -> Environment {
        if self.production {
            Environment::Production
        } else if self.test {
            Environment::Test
        } else {
            default
        }
    }
}

/// Entry point called from `main`.
pub fn run(cli: Cli) -> Result<()> {
    let cwd = std::env::current_dir().map_err(|e| Error::io(".", e))?;

    match cli.command {
        Command::Init => init(&cwd),

        Command::Reads(args) => {
            let cfg = Config::load(&cwd)?;
            let mode = args.mode.resolve();
            let env = args.env.resolve(cfg.default_environment);
            tracing::info!(input = %args.input.display(), mode = mode.flag(), env = %env, "reads submit");
            submit_reads(&cwd, &cfg, &args.input, args.input_dir.as_deref(), mode, env)
        }

        Command::Assembly(args) => {
            let cfg = Config::load(&cwd)?;
            let mode = args.mode.resolve();
            let env = args.env.resolve(cfg.default_environment);
            tracing::info!(input = %args.input.display(), mode = mode.flag(), env = %env, "assembly submit");
            submit_assemblies(&cwd, &cfg, &args.input, args.input_dir.as_deref(), mode, env)
        }

        Command::Mag(MagCommand::Prepare { input, output }) => {
            tracing::info!(input = %input.display(), output = %output.display(), "mag prepare");
            crate::mag_tsv::prepare(&input, &output)
        }

        Command::Mag(MagCommand::Submit {
            input,
            samples,
            input_dir,
            mode,
            env,
        }) => {
            let cfg = Config::load(&cwd)?;
            let mode = mode.resolve();
            let env = env.resolve(cfg.default_environment);
            tracing::info!(input = %input.display(), samples = %samples.display(), mode = mode.flag(), env = %env, "mag submit");
            submit_mags(&cwd, &cfg, &input, &samples, input_dir.as_deref(), mode, env)
        }

        Command::Status => {
            tracing::info!("status");
            let history = crate::history::History::at(&cwd);
            let records = history.read()?;
            print!("{}", crate::history::render(&records));
            Ok(())
        }
    }
}

/// Validate/submit each read run, appending a history record per object.
fn submit_reads(
    cwd: &Path,
    cfg: &Config,
    input: &Path,
    input_dir: Option<&Path>,
    mode: SubmitMode,
    env: Environment,
) -> Result<()> {
    let creds = webin::preflight(cfg)?;
    let records = crate::input::read_reads(input)?;
    let history = History::at(cwd);
    let mut summary = Summary::default();
    for rec in &records {
        let manifest = manifest::reads_manifest(rec);
        let run = WebinRun {
            context: Context::Reads,
            name: rec.name.as_str(),
            manifest: manifest.as_str(),
            mode,
            environment: env,
            input_dir,
        };
        // Every error in here is run-level, so it aborts the remaining objects — but only after
        // reporting what already landed.
        if let Err(e) = run_object(cfg, &history, &mut summary, &run, creds) {
            return Err(abort_run(summary, mode, e));
        }
    }
    finish_run(summary, mode)
}

/// Validate/submit each genome assembly, appending a history record per object.
fn submit_assemblies(
    cwd: &Path,
    cfg: &Config,
    input: &Path,
    input_dir: Option<&Path>,
    mode: SubmitMode,
    env: Environment,
) -> Result<()> {
    let creds = webin::preflight(cfg)?;
    let records = crate::input::read_assemblies(input)?;
    let history = History::at(cwd);
    let mut summary = Summary::default();
    for rec in &records {
        let manifest = manifest::genome_manifest(rec);
        let run = WebinRun {
            context: Context::Genome,
            name: rec.assemblyname.as_str(),
            manifest: manifest.as_str(),
            mode,
            environment: env,
            input_dir,
        };
        // Every error in here is run-level, so it aborts the remaining objects — but only after
        // reporting what already landed.
        if let Err(e) = run_object(cfg, &history, &mut summary, &run, creds) {
            return Err(abort_run(summary, mode, e));
        }
    }
    finish_run(summary, mode)
}

/// Validate/submit each MAG bin under `-context genome`, resolving its derived sample from the
/// `bin_name -> ERS…` mapping and applying the single-contig chromosome fallback (ADR 0006).
fn submit_mags(
    cwd: &Path,
    cfg: &Config,
    input: &Path,
    samples: &Path,
    input_dir: Option<&Path>,
    mode: SubmitMode,
    env: Environment,
) -> Result<()> {
    let creds = webin::preflight(cfg)?;
    let bins = crate::input::read_mag_assemblies(input)?;
    let map = crate::input::read_sample_map(samples)?;

    // Fail fast if any bin has no registered sample, before invoking Webin-CLI on any of them.
    let missing: Vec<&str> = bins
        .iter()
        .filter(|b| !map.contains_key(&b.bin_name))
        .map(|b| b.bin_name.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(Error::Input {
            path: samples.to_path_buf(),
            message: format!("no registered sample_accession for bin(s): {}", missing.join(", ")),
        });
    }

    let history = History::at(cwd);
    let mut summary = Summary::default();
    for bin in &bins {
        let sample = &map[&bin.bin_name];
        let assembly = match build_mag_assembly(bin, sample, input_dir) {
            Ok(assembly) => assembly,
            Err(e) => return Err(abort_run(summary, mode, e)),
        };
        let manifest = manifest::genome_manifest(&assembly);
        let run = WebinRun {
            context: Context::Genome,
            name: bin.assemblyname.as_str(),
            manifest: manifest.as_str(),
            mode,
            environment: env,
            input_dir,
        };
        // Every error in here is run-level, so it aborts the remaining objects — but only after
        // reporting what already landed.
        if let Err(e) = run_object(cfg, &history, &mut summary, &run, creds) {
            return Err(abort_run(summary, mode, e));
        }
    }
    finish_run(summary, mode)
}

/// Build the genome [`AssemblyRecord`] for one MAG bin: fixed MAG assembly type, the resolved
/// sample, the FASTA, and — when the bin is a single contig — a generated `CHROMOSOME_LIST`.
fn build_mag_assembly(
    bin: &MagBin,
    sample: &str,
    input_dir: Option<&Path>,
) -> Result<AssemblyRecord> {
    let mut files = vec![AssemblyFile {
        kind: "FASTA".to_string(),
        path: bin.fasta.clone(),
    }];

    // Detection reads the FASTA on disk (resolved against input_dir); the manifest keeps the
    // input_dir-relative paths that Webin-CLI expects.
    let fasta_disk = resolve(input_dir, &bin.fasta);
    let chromosome_name = bin.chromosome_name.clone().unwrap_or_else(|| bin.bin_name.clone());
    if let Some(entry) = chromosome::single_contig_entry(
        &fasta_disk,
        &chromosome_name,
        DEFAULT_CHROMOSOME_TYPE,
        bin.topology,
    )? {
        let list_rel = chromosome_list_path(&bin.fasta, &bin.assemblyname);
        let list_disk = resolve(input_dir, &list_rel);
        let content = chromosome::render_chromosome_list(&[entry]);
        chromosome::write_chromosome_list_gz(&list_disk, &content)?;
        tracing::info!(bin = %bin.bin_name, "single contig: submitting as chromosome");
        files.push(AssemblyFile {
            kind: "CHROMOSOME_LIST".to_string(),
            path: list_rel,
        });
    }

    Ok(AssemblyRecord {
        assemblyname: bin.assemblyname.clone(),
        study: bin.study.clone(),
        sample: sample.to_string(),
        assembly_type: MAG_ASSEMBLY_TYPE.to_string(),
        coverage: bin.coverage.clone(),
        program: bin.program.clone(),
        platform: bin.platform.clone(),
        moleculetype: None,
        mingaplength: None,
        description: bin.description.clone(),
        run_ref: bin.run_ref.clone(),
        files,
    })
}

/// Resolve a manifest-relative path against `input_dir` (if any) to a real on-disk path.
fn resolve(input_dir: Option<&Path>, rel: &Path) -> PathBuf {
    match input_dir {
        Some(dir) => dir.join(rel),
        None => rel.to_path_buf(),
    }
}

/// The chromosome list file path for a bin: beside its FASTA (same directory), named from the
/// assembly so it is unique and stable. Returned relative to `input_dir`, matching the FASTA.
fn chromosome_list_path(fasta: &Path, assemblyname: &str) -> PathBuf {
    let file = format!("{}.chromosome_list.txt.gz", webin::sanitize(assemblyname));
    match fasta.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file),
        _ => PathBuf::from(file),
    }
}

/// Running tally of a submission run's per-object outcomes.
#[derive(Default)]
struct Summary {
    ok: usize,
    failed: usize,
}

impl Summary {
    fn record(&mut self, r: &Record) {
        match r.outcome {
            Outcome::Success => self.ok += 1,
            Outcome::Failure => self.failed += 1,
        }
    }
}

/// Validate/submit one object and record the outcome. Returning `Err` means the *run* cannot
/// continue (Webin-CLI unusable, account rejected, history unwritable) — never that this object
/// merely failed validation, which is captured in the recorded outcome instead. Keeping the tally
/// and the history append together here means an aborted run reports the same numbers it wrote.
fn run_object(
    cfg: &Config,
    history: &History,
    summary: &mut Summary,
    run: &WebinRun,
    creds: webin::Credentials<'_>,
) -> Result<()> {
    let outcome = webin::submit_object(cfg, run, creds)?;
    summary.record(&outcome);
    history.append(&outcome)
}

/// Past-tense verb describing what a run in `mode` did to its objects.
fn verb(mode: SubmitMode) -> &'static str {
    match mode {
        SubmitMode::Validate => "validated",
        SubmitMode::Submit => "submitted",
    }
}

/// Report what a run achieved before a run-level error cut it short, then hand the error back. A
/// `--submit` run that aborts partway has already sent real objects to ENA and written them to the
/// history; saying so keeps the user from having to reconstruct it (or resubmitting by mistake).
fn abort_run(summary: Summary, mode: SubmitMode, err: Error) -> Error {
    // Nothing was attempted (the run died on its first object), so there is no partial run to
    // report — saying "0 ok, 0 failed" would imply objects were processed when none were.
    if summary.ok + summary.failed > 0 {
        println!(
            "aborted — {}: {} ok, {} failed before stopping",
            verb(mode),
            summary.ok,
            summary.failed
        );
    }
    err
}

/// Print the run summary and turn any failures into a non-zero exit.
fn finish_run(summary: Summary, mode: SubmitMode) -> Result<()> {
    let verb = verb(mode);
    println!("{verb}: {} ok, {} failed", summary.ok, summary.failed);
    if summary.failed > 0 {
        Err(Error::SubmissionFailed {
            failed: summary.failed,
        })
    } else {
        Ok(())
    }
}

/// Files written by `init`, relative to the working directory.
const TEMPLATE_FILES: &[(&str, &str)] = &[
    ("templates/reads.tsv", READS_TEMPLATE),
    ("templates/assemblies.tsv", ASSEMBLIES_TEMPLATE),
    ("templates/mag_samples.tsv", MAG_SAMPLES_TEMPLATE),
    ("templates/mag_assemblies.tsv", MAG_ASSEMBLIES_TEMPLATE),
    ("templates/registered_mags.tsv", REGISTERED_MAGS_TEMPLATE),
];

/// Scaffold `ena-submit.toml` and the template TSVs, skipping any file that already exists.
fn init(dir: &Path) -> Result<()> {
    write_if_absent(&dir.join(CONFIG_FILE), CONFIG_TEMPLATE)?;
    for (rel, contents) in TEMPLATE_FILES {
        write_if_absent(&dir.join(rel), contents)?;
    }
    println!(
        "Initialised ena-submit project.\n\
         - {CONFIG_FILE}: edit paths / set credentials via WEBIN_USERNAME & WEBIN_PASSWORD\n\
         - templates/: copy a template, fill it in, then run `ena-submit <reads|assembly|mag>`"
    );
    Ok(())
}

/// Write `contents` to `path`, creating parent dirs. Existing files are left untouched (reported).
fn write_if_absent(path: &Path, contents: &str) -> Result<()> {
    if path.exists() {
        println!("  exists, skipped: {}", path.display());
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    std::fs::write(path, contents).map_err(|e| Error::io(path, e))?;
    println!("  wrote: {}", path.display());
    Ok(())
}

const READS_TEMPLATE: &str = "\
name\tstudy\tsample\tplatform\tinstrument\tlibrary_source\tlibrary_selection\tlibrary_strategy\tlibrary_name\tinsert_size\tdescription\tfastq1\tfastq2
run1\tPRJEB00000\tERS0000000\tILLUMINA\tIllumina NovaSeq 6000\tGENOMIC\tRANDOM\tWGS\tlib1\t350\tExample paired-end run\treads_1.fastq.gz\treads_2.fastq.gz
";

const ASSEMBLIES_TEMPLATE: &str = "\
assemblyname\tstudy\tsample\tassembly_type\tcoverage\tprogram\tplatform\tmoleculetype\tmingaplength\tdescription\trun_ref\tfasta
asm_isolate1\tPRJEB00000\tERS0000000\tclone or isolate\t30\tSPAdes\tILLUMINA\tgenomic DNA\t100\tExample isolate assembly\tERR0000000\tassembly.fasta.gz
";

// Example MAG *sample sheet*: you fill everything except `tax_id`, pasting GTDB-Tk's own
// `classification` and `fastani_reference` values into `scientific_name` and `GTDBtk fastani Ref`.
// `ena-submit mag prepare` fills `tax_id` and rewrites `scientific_name` to the matching ENA name.
// Extra checklist columns are passed through unchanged.
const MAG_SAMPLES_TEMPLATE: &str = "\
sample_alias\ttax_id\tscientific_name\tGTDBtk fastani Ref\tsample derived from\tenvironment (biome)\tcompleteness score\tcontamination score
bin.1\t\td__Bacteria;p__Bacteroidota;c__Bacteroidia;o__Bacteroidales;f__Bacteroidaceae;g__Phocaeicola;s__Phocaeicola vulgatus\tGCF_000012825.1\tERS1111111\thuman gut\t95.5\t2.1
";

// Assembly parameters for `ena-submit mag submit`. `sample` is filled from registered_mags.tsv and
// `assembly_type` is set to the MAG value automatically, so neither appears here. `topology`
// (linear/circular) and `chromosome_name` are optional and only apply to single-contig bins, which
// are submitted as chromosomes; leave them blank for multi-contig bins.
const MAG_ASSEMBLIES_TEMPLATE: &str = "\
bin_name\tassemblyname\tstudy\tcoverage\tprogram\tplatform\tfasta\trun_ref\tdescription\ttopology\tchromosome_name
bin.1\tMAG_bin.1\tPRJEB00000\t25\tmetaSPAdes\tILLUMINA\tbins/bin.1.fasta.gz\tERR0000000\tMAG derived from ERS1111111\t\t
";

// Mapping produced after you upload the completed sample sheet and receive accessions.
const REGISTERED_MAGS_TEMPLATE: &str = "\
bin_name\tsample_accession
bin.1\tERS2222222
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chromosome::Topology;

    fn bin(fasta: PathBuf, topology: Topology) -> MagBin {
        MagBin {
            bin_name: "bin.1".into(),
            assemblyname: "MAG bin.1".into(),
            study: "PRJEB1".into(),
            coverage: "25".into(),
            program: "metaSPAdes".into(),
            platform: "ILLUMINA".into(),
            fasta,
            run_ref: None,
            description: None,
            topology,
            chromosome_name: None,
        }
    }

    #[test]
    fn chromosome_list_path_sits_beside_fasta_with_sanitized_name() {
        let p = chromosome_list_path(Path::new("bins/bin.1.fa.gz"), "MAG bin.1");
        assert_eq!(p, PathBuf::from("bins/MAG_bin.1.chromosome_list.txt.gz"));

        // No parent directory -> bare file name.
        let p = chromosome_list_path(Path::new("bin.fa.gz"), "asm1");
        assert_eq!(p, PathBuf::from("asm1.chromosome_list.txt.gz"));
    }

    #[test]
    fn build_mag_assembly_multi_contig_is_fasta_only() {
        let dir = tempfile::tempdir().unwrap();
        let fasta = dir.path().join("bin.fa");
        std::fs::write(&fasta, b">c1\nACGT\n>c2\nTTTT\n").unwrap();

        let b = bin(fasta.clone(), Topology::Linear);
        let asm = build_mag_assembly(&b, "ERS999", None).unwrap();

        assert_eq!(asm.assembly_type, MAG_ASSEMBLY_TYPE);
        assert_eq!(asm.sample, "ERS999");
        let kinds: Vec<&str> = asm.files.iter().map(|f| f.kind.as_str()).collect();
        assert_eq!(kinds, ["FASTA"]);
    }

    #[test]
    fn build_mag_assembly_single_contig_adds_chromosome_list() {
        let dir = tempfile::tempdir().unwrap();
        // input_dir-relative FASTA; the file lives under input_dir on disk.
        let input_dir = dir.path();
        std::fs::write(input_dir.join("bin.fa"), b">tig1\nACGTACGT\n").unwrap();

        let b = bin(PathBuf::from("bin.fa"), Topology::Circular);
        let asm = build_mag_assembly(&b, "ERS999", Some(input_dir)).unwrap();

        let kinds: Vec<&str> = asm.files.iter().map(|f| f.kind.as_str()).collect();
        assert_eq!(kinds, ["FASTA", "CHROMOSOME_LIST"]);

        // The manifest carries the input_dir-relative list path, and the gzipped file exists on disk.
        let list = &asm.files[1].path;
        assert_eq!(list, &PathBuf::from("MAG_bin.1.chromosome_list.txt.gz"));
        let disk = input_dir.join(list);
        assert!(disk.exists(), "chromosome list not written to {}", disk.display());
        assert_eq!(&std::fs::read(&disk).unwrap()[..2], &[0x1f, 0x8b]); // gzip magic
    }
}
