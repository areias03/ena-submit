# Architecture — `ena-submit`

A Rust CLI that turns tabular (TSV) metadata into valid submissions to the European Nucleotide
Archive (ENA) for **reads**, **genome assemblies**, and **MAGs**. It is a thin, well-tested
orchestration layer over the official Java **Webin-CLI** — not a reimplementation of ENA's
validation or file transfer.

## Responsibilities

- Parse and validate TSV input (one row per run / assembly / MAG bin).
- Generate Webin-CLI **manifest** files (`reads` and `genome` contexts).
- Complete the user's **MAG sample sheet** by filling its `tax_id` column (resolved from
  `scientific_name` via the ENA taxonomy API); all other columns pass through unchanged.
- Invoke Webin-CLI with the right flags and parse the receipt XML for accessions.
- Record an auditable local submission history.

## What is out of scope

- Creating the **study** and **non-MAG samples** — the user does this manually; the tool only
  references them by accession/alias.
- Registering MAG samples via XML — the tool emits a TSV only; the user uploads it and returns the
  resulting `ERS…` accessions.
- Native ENA validation or FTP/Aspera transfer — delegated entirely to Webin-CLI.

## Module map (`src/`)

| Module        | Responsibility |
|---------------|----------------|
| `main.rs`     | Logging setup + call `cli::run`. |
| `cli.rs`      | `clap` subcommands + dispatch. |
| `config.rs`   | Global config (`ena-submit.toml` + env): credentials, test/prod default, jar/java paths. |
| `model.rs`    | Domain types: `ReadRecord`, `AssemblyRecord`, `MagBin`, enums, `Context`, `SubmitMode`. |
| `input.rs`    | Generic order-preserving `Table` + typed reads/assembly readers with row-level validation. |
| `manifest.rs` | Render reads/genome manifest files (one per object). |
| `mag_tsv.rs`  | Fill the `tax_id` column of a MAG sample sheet via the ENA taxonomy API. |
| `chromosome.rs` | Detect single-contig MAG bins (gzip-aware FASTA scan) and render/write the chromosome list file for chromosome-level submission. |
| `webin.rs`    | Shell out to `java -jar webin-cli.jar …`; preflight Java 17+/jar checks. |
| `receipt.rs`  | Parse receipt XML → accessions + status. |
| `history.rs`  | Append-only local state (`.ena-submit/history.jsonl`). |
| `error.rs`    | `thiserror` error enum (`anyhow` at the binary boundary). |

## CLI surface

```
ena-submit init
ena-submit reads    <input.tsv> [--validate|--submit] [--test] [--input-dir DIR]
ena-submit assembly <input.tsv> [--validate|--submit] [--test] [--input-dir DIR]
ena-submit mag prepare <mags.tsv> [--checklist ERC000050] -o mag_samples.tsv
ena-submit mag submit  <mags.tsv> --samples registered_mags.tsv [--validate|--submit] [--test]
ena-submit status
```

## MAG submission flow

1. `mag prepare` — user's near-complete sample sheet → same sheet with `tax_id` filled from
   `scientific_name` (ENA taxonomy API).
2. User uploads the completed sheet via the Webin spreadsheet UI → obtains `ERS…` accessions →
   saves `registered_mags.tsv` (`bin_name → ERS…`).
3. `mag submit` — per bin: genome manifest with `ASSEMBLY_TYPE="Metagenome-Assembled Genome (MAG)"`
   and the `ERS…` sample (from the mapping) → Webin-CLI `genome` → receipt → history.
   - **Single-contig fallback**: if the bin's FASTA holds exactly one sequence (e.g. a closed
     long-read genome), it is submitted as a **chromosome** — a gzipped chromosome list file
     (`CHROMOSOME_LIST`) is generated alongside the FASTA, with topology from the input (default
     linear, `circular` when marked). Multi-contig bins submit as contigs. See ADR 0006.

## External dependencies

- **Webin-CLI jar** (`enasequence/webin-cli`) and **Java 17+** at runtime.
- **ENA taxonomy REST API** (`www.ebi.ac.uk/ena/taxonomy/rest`) — reached over HTTPS by `mag prepare`
  only, via the blocking `ureq` client.
- Crates: `clap`, `serde`, `serde_json`, `csv`, `toml`, `quick-xml`, `thiserror`, `anyhow`,
  `tracing`, `time`, `regex`, `ureq`, `flate2`.

See [`docs/adr/`](adr/) for the reasoning behind these choices.
