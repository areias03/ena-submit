//! Command-line interface: argument parsing and subcommand dispatch.
//!
//! Milestone 2 implements `init` (scaffolding) fully; the submission subcommands are wired into the
//! CLI surface but return [`Error::NotImplemented`] until later milestones.

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use crate::config::{CONFIG_FILE, CONFIG_TEMPLATE, Config};
use crate::error::{Error, Result};
use crate::model::{Environment, SubmitMode};

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
            let _ = (&cfg, &args.input_dir);
            Err(Error::NotImplemented("reads submission (milestone 7)"))
        }

        Command::Assembly(args) => {
            let cfg = Config::load(&cwd)?;
            let mode = args.mode.resolve();
            let env = args.env.resolve(cfg.default_environment);
            tracing::info!(input = %args.input.display(), mode = mode.flag(), env = %env, "assembly submit");
            let _ = (&cfg, &args.input_dir);
            Err(Error::NotImplemented("assembly submission (milestone 7)"))
        }

        Command::Mag(MagCommand::Prepare { input, output }) => {
            tracing::info!(input = %input.display(), output = %output.display(), "mag prepare");
            Err(Error::NotImplemented("mag prepare: tax_id fill (milestone 4)"))
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
            let _ = (&cfg, &input_dir);
            Err(Error::NotImplemented("mag submission (milestone 7)"))
        }

        Command::Status => {
            tracing::info!("status");
            Err(Error::NotImplemented("status (milestone 6)"))
        }
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

// Example MAG *sample sheet*: you fill everything except `tax_id`; `ena-submit mag prepare`
// resolves `tax_id` from `scientific_name`. Extra checklist columns are passed through unchanged.
const MAG_SAMPLES_TEMPLATE: &str = "\
sample_alias\ttax_id\tscientific_name\tsample derived from\tenvironment (biome)\tcompleteness score\tcontamination score
bin.1\t\tuncultured Bacteroides sp.\tERS1111111\thuman gut\t95.5\t2.1
";

// Assembly parameters for `ena-submit mag submit`. `sample` is filled from registered_mags.tsv and
// `assembly_type` is set to the MAG value automatically, so neither appears here.
const MAG_ASSEMBLIES_TEMPLATE: &str = "\
bin_name\tassemblyname\tstudy\tcoverage\tprogram\tplatform\tfasta\trun_ref\tdescription
bin.1\tMAG_bin.1\tPRJEB00000\t25\tmetaSPAdes\tILLUMINA\tbins/bin.1.fasta.gz\tERR0000000\tMAG derived from ERS1111111
";

// Mapping produced after you upload the completed sample sheet and receive accessions.
const REGISTERED_MAGS_TEMPLATE: &str = "\
bin_name\tsample_accession
bin.1\tERS2222222
";
