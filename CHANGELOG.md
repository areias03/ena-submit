# Changelog

All notable changes to `ena-submit` are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Project planning and architecture record: development plan, architecture doc, and
  ADRs 0001–0003 establishing the hybrid (Webin-CLI wrapper) approach, TSV input, and
  MAG-sample-TSV-only scope.
- Core CLI skeleton (milestone 2): `clap` command surface (`init`, `reads`, `assembly`,
  `mag prepare`, `mag submit`, `status`) with mutually-exclusive `--validate`/`--submit`
  and `--test`/`--production` flags; `error` (typed errors via `thiserror`), `config`
  (layered `ena-submit.toml` + env credentials), and `model` (domain records/enums) modules;
  `tracing`-based logging. `init` scaffolds `ena-submit.toml` and template TSVs idempotently.
  Submission subcommands are stubbed pending later milestones.
- Input layer (milestone 3): `input` module with a generic order-preserving `Table` TSV reader
  (used for the MAG sample sheet and the `bin_name→accession` mapping) plus typed, fully-validated
  `read_reads` / `read_assemblies` readers — required-column and required-value checks, `ASSEMBLYNAME`
  pattern/length, read-file rules (single kind, paired FASTQ), with all row problems aggregated into
  one error. 14 unit tests. `init` now emits `mag_samples.tsv` and `mag_assemblies.tsv` templates.
- MAG sample preparation (milestone 4): `mag prepare` is now implemented. `mag_tsv` reads the
  user's sample sheet with the generic `Table` reader, resolves each row's `scientific_name` to a
  taxon id via the ENA taxonomy REST API (new blocking `ureq` HTTP dependency), and writes the
  completed TSV with all other columns preserved in order. Already-filled `tax_id` cells are kept
  (idempotent re-runs); unknown, ambiguous, and non-submittable names are aggregated into one
  error, while transport/HTTP failures surface as a new `Error::Network`. 10 unit tests behind a
  `TaxonomyResolver` trait (offline-testable). Implements ADR 0004.
- Manifest rendering (milestone 5): `manifest` module with `reads_manifest` / `genome_manifest`,
  turning a validated `ReadRecord` / `AssemblyRecord` into Webin-CLI `KEY<TAB>value` manifest text
  (one file per object). Mandatory `reads` and `genome` fields are always emitted; optional fields
  (`LIBRARY_NAME`, `INSERT_SIZE`, `DESCRIPTION`, `MOLECULETYPE`, `MINGAPLENGTH`, `RUN_REF`) only when
  set; data files render one line per Webin-CLI file-type keyword. Pure functions, 9 unit tests.
  Consumed by the submission layer (milestone 7).
- Test coverage: unit tests for the `config` layer (defaults, file loading, malformed/unknown TOML,
  credential requirements) and the `mag_tsv` TSV writer, plus a `tests/cli.rs` integration suite
  that drives the compiled binary offline (`init` scaffolding/idempotency, `mag prepare`
  missing-column error, submission preflight guards, `status` rendering, unknown-subcommand usage
  errors). Adds the `tempfile` dev-dependency.
- Single-contig MAG chromosome support (milestone 8): `chromosome` module with a gzip-aware FASTA
  sequence scan (`sequence_names`, `single_contig_name`) and chromosome list file rendering/writing
  (`render_chromosome_list`, `write_chromosome_list_gz`, `Topology`). A MAG bin whose FASTA holds a
  single contig can be submitted as a chromosome (gzipped `CHROMOSOME_LIST` alongside the FASTA,
  linear/circular topology) rather than as anonymous contigs; multi-contig bins are unchanged. Pure,
  offline unit tests (7); new `flate2` dependency. `mag submit` wires this in (milestone 7).
  Implements ADR 0006.
- Submission history and `status` (milestone 6): new `history` module implementing the append-only
  JSONL store at `.ena-submit/history.jsonl` (one `Record` per submission attempt: timestamp,
  context, name, mode, environment, outcome, plus optional accessions/receipt/error). `History`
  appends records (creating the parent dir) and reads them back forward-compatibly — unknown fields
  are ignored, a missing file is an empty history, and an unparseable line is reported with its line
  number. `ena-submit status` now renders recorded submissions (or reports none). `Context` and
  `SubmitMode` gained serde derives; new shared `Accession` domain type. 10 unit tests plus two
  `tests/cli.rs` cases. New `serde_json` and `time` dependencies. Implements ADR 0007.
- Submission via Webin-CLI (milestone 7): the `reads`, `assembly`, and `mag submit` commands are now
  implemented end-to-end. New `webin` module shells out to `java -jar webin-cli.jar` (preflighting
  Java 17+ and the jar, building the argument list, writing the manifest under `output_dir`) and new
  `receipt` module parses the returned receipt XML (via `quick-xml`) into accessions and error
  messages. Each object is validated/submitted one at a time and recorded in the history; failures
  are captured per object and surface as a non-zero exit. `mag submit` resolves each bin's derived
  sample from the `bin_name -> ERS…` mapping, sets the MAG assembly type, and applies the
  single-contig chromosome fallback (writing a gzipped `CHROMOSOME_LIST` beside the FASTA) from ADR
  0006. New `read_mag_assemblies` input reader and `MagBin` domain type. New `quick-xml` dependency;
  the obsolete `Error::NotImplemented` variant was removed. Adds unit tests for receipt parsing,
  Webin-CLI arg/version/receipt handling, the MAG reader, and the MAG-assembly builder, plus
  integration tests for the credential and jar preflight checks.
- Architecture decision records: ADR 0005 (blocking HTTP via `ureq`, recorded retroactively for
  milestone 4), ADR 0006 (single-contig MAGs as chromosomes), and ADR 0007 (append-only JSONL
  submission history for `status`, now accepted and implemented).
- Documentation: all planned commands (`init`, `reads`, `assembly`, `mag prepare`, `mag submit`,
  `status`) are now implemented. `docs/architecture.md` gains a Status section, and ADRs 0004/0005/
  0006 were refreshed to drop forward-looking "later milestone / future" phrasing now that the
  submission path and MAG chromosome wiring have landed.

### Changed
- MAG sample handling: `mag prepare` now **completes a user-provided sample sheet** by filling the
  `tax_id` column (resolved from `scientific_name` via the ENA taxonomy API) instead of generating
  the sheet from bin metadata. Recorded in ADR 0004, superseding the MAG-sample half of ADR 0003.
- Credential validation is now consolidated in a single place: `webin::preflight` checks the
  credentials (before the jar and Java checks) and returns a `Credentials` value that is passed to
  each `webin::submit_object` call, replacing the duplicate check the submission path performed per
  object. `Credentials` deliberately does not implement `Debug` so the password cannot leak into
  logs or panic messages.
- The Webin password is now handed to Webin-CLI through an environment variable
  (`-passwordEnv=ENA_SUBMIT_WEBIN_PASSWORD`) instead of `-password <secret>` on its command line,
  where any local user could read it from `ps` or `/proc/<pid>/cmdline` for the duration of a
  submission. Verified against Webin-CLI 9.0.3.
- A run that aborts partway (currently: a rejected account) now prints what it managed to do first
  — `aborted — submitted: 7 ok, 0 failed before stopping` — so a partial `--submit` run does not
  leave the user reconstructing from the history file which objects already reached ENA.
- `Credentials` fields are private and constructible only by `preflight`, so no future call site can
  hand `submit_object` credentials that were never validated.
- Failing to clear a stale `webin-cli.report` is now a hard error instead of being ignored: an
  undeletable report carrying the auth message would otherwise make the *next* object's ordinary
  failure look like a rejected account and abort a healthy run.
- A rejected Webin account now aborts the run on the first object instead of being recorded as an
  ordinary per-object failure. Webin-CLI exits with the same status code for every error class, so
  the run-level `<output_dir>/webin-cli.report` (cleared before each invocation, so a stale one can
  never abort a good run) is checked for its authentication message and turned into the new
  `Error::InvalidCredentials`. Previously a wrong password launched a doomed JVM per input row and
  reported N failures with the generic "validation failed" message; nothing is appended to the
  history, since a rejected account is not an object outcome.
- A TSV with a header but no data rows is now an input error ("no data rows") instead of a silent
  success. The check lives in `input::Table::parse`, so every command that reads a TSV (`reads`,
  `assembly`, `mag prepare`, `mag submit`) rejects an unfilled template rather than reporting a
  no-op run as successful.

[Unreleased]: https://github.com/areias03/ena-submit/compare/HEAD...HEAD
