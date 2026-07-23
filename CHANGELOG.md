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
  Consumed by the submission layer in a later milestone.
- Test coverage: unit tests for the `config` layer (defaults, file loading, malformed/unknown TOML,
  credential requirements) and the `mag_tsv` TSV writer, plus a `tests/cli.rs` integration suite
  that drives the compiled binary offline (`init` scaffolding/idempotency, `mag prepare`
  missing-column error, not-implemented submission stubs, unknown-subcommand usage errors). Adds the
  `tempfile` dev-dependency.

### Changed
- MAG sample handling: `mag prepare` now **completes a user-provided sample sheet** by filling the
  `tax_id` column (resolved from `scientific_name` via the ENA taxonomy API) instead of generating
  the sheet from bin metadata. Recorded in ADR 0004, superseding the MAG-sample half of ADR 0003.

[Unreleased]: https://github.com/areias03/ena-submit/compare/HEAD...HEAD
