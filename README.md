# ena-submit

A Rust CLI that turns tabular (TSV) metadata into valid submissions to the European Nucleotide
Archive (ENA) for **reads**, **genome assemblies**, and **MAGs**.

It is a thin, well-tested orchestration layer over the official Java
[**Webin-CLI**](https://github.com/enasequence/webin-cli) — it does not reimplement ENA's
validation or file transfer. `ena-submit` parses and validates your input, generates the right
manifests, fills the pieces ENA can compute for you (e.g. MAG sample `tax_id`s), invokes Webin-CLI,
and records an auditable local history.

## Status

Early development. Implemented so far:

- `init` — scaffold `ena-submit.toml` and template input TSVs.
- Typed, fully-validated TSV input for reads and assemblies.
- `mag prepare` — complete a MAG sample sheet by resolving each row's `scientific_name` to a
  `tax_id` via the ENA taxonomy API.

Submission (`reads`, `assembly`, `mag submit`), `status`, and receipt handling are in progress.

## Requirements

- Rust (edition 2024).
- For actual submission: **Java 17+** and the **Webin-CLI** jar (not needed for `mag prepare`).

## Install

```sh
cargo build --release
# binary at target/release/ena-submit
```

## Usage

```
ena-submit init
ena-submit reads    <input.tsv> [--validate|--submit] [--test] [--input-dir DIR]
ena-submit assembly <input.tsv> [--validate|--submit] [--test] [--input-dir DIR]
ena-submit mag prepare <mags.tsv> -o mag_samples.filled.tsv
ena-submit mag submit  <mags.tsv> --samples registered_mags.tsv [--validate|--submit] [--test]
ena-submit status
```

Submission commands default to **validate-only** and target the **test** service unless you pass
`--submit` / `--production`.

### MAG workflow

1. `mag prepare <mags.tsv> -o mag_samples.filled.tsv` — fills the `tax_id` column from
   `scientific_name`; all other checklist columns pass through unchanged.
2. Upload the completed sheet via the Webin spreadsheet UI to obtain `ERS…` sample accessions, and
   save a `bin_name → ERS…` mapping (`registered_mags.tsv`).
3. `mag submit <mags.tsv> --samples registered_mags.tsv` — submits each MAG assembly.

## Configuration

`ena-submit.toml` (created by `init`) holds paths and the default test/production choice.
Credentials come from `WEBIN_USERNAME` / `WEBIN_PASSWORD` (or the config file). Never commit
credentials — the provided `.gitignore` excludes `ena-submit.toml` and runtime state.

## Documentation

See [`docs/architecture.md`](docs/architecture.md) and the architecture decision records in
[`docs/adr/`](docs/adr/).

## License

TBD.
