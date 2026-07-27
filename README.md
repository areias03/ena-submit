# ena-submit

A Rust CLI that turns tabular (TSV) metadata into valid submissions to the European Nucleotide
Archive (ENA) for **reads**, **genome assemblies**, and **MAGs**.

It is a thin, well-tested orchestration layer over the official Java
[**Webin-CLI**](https://github.com/enasequence/webin-cli) — it does not reimplement ENA's
validation or file transfer. `ena-submit` parses and validates your input, generates the right
manifests, fills the pieces ENA can compute for you (e.g. MAG sample `tax_id`s), invokes Webin-CLI,
and records an auditable local history.

## Status

All planned commands are implemented:

- `init` — scaffold `ena-submit.toml` and template input TSVs.
- Typed, fully-validated TSV input for reads, assemblies, and MAG bins.
- `mag prepare` — complete a MAG sample sheet by resolving each row's `scientific_name` to a
  `tax_id` via the ENA taxonomy API, falling back to `"<genus> sp."` for genus-only names and GTDB
  placeholders.
- `reads` / `assembly` / `mag submit` — render manifests and drive Webin-CLI to validate or submit,
  parsing the receipt for accessions. `mag submit` submits single-contig bins as chromosomes.
- `status` — render the local append-only submission history (`.ena-submit/history.jsonl`).

Actual validation/submission requires Java 17+ and the Webin-CLI jar (see Requirements).

## Requirements

- Rust (edition 2024).
- For actual submission: **Java 17+** and the **Webin-CLI** jar (not needed for `mag prepare`).
  Webin-CLI **1.8.12 or newer** is required, since the password is passed via `-passwordEnv`;
  verified against 9.0.3. Any current release is fine — 1.8.12 dates from 2019.

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

Every input TSV must contain at least one data row: a file with only a header is rejected, so an
unfilled template fails loudly instead of reporting a run with nothing to do as a success.

### MAG workflow

1. `mag prepare <mags.tsv> -o mag_samples.filled.tsv` — fills the `tax_id` column from
   `scientific_name`; all other checklist columns pass through unchanged.

   Two common name shapes cannot be submitted as written, and are retried as `"<genus> sp."`:

   | `scientific_name` in the sheet | why it fails | retried as |
   | --- | --- | --- |
   | `Bacteroides` | ENA has the genus but marks it *not submittable* | `Bacteroides sp.` |
   | `Phocaeicola sp900556845` | GTDB accessioned epithet; ENA has no such name | `Phocaeicola sp.` |

   The retry happens only after the name as written fails, so a name ENA does accept is never
   second-guessed. When the retry succeeds the `scientific_name` cell is rewritten to the resolved
   name — ENA validates `scientific_name` against `tax_id`, so the two have to agree. This is the
   only case where `mag prepare` edits a column other than `tax_id`, and it reports how many cells
   it rewrote. Real binomials (`Phocaeicola vulgatus`) are never rewritten.
2. Upload the completed sheet via the Webin spreadsheet UI to obtain `ERS…` sample accessions, and
   save a `bin_name → ERS…` mapping (`registered_mags.tsv`).
3. `mag submit <mags.tsv> --samples registered_mags.tsv` — submits each MAG assembly.

## Configuration

`ena-submit.toml` (created by `init`) holds paths and the default test/production choice.
Credentials come from `WEBIN_USERNAME` / `WEBIN_PASSWORD` (or the config file). Never commit
credentials — the provided `.gitignore` excludes `ena-submit.toml` and runtime state.

The password is passed to Webin-CLI through an environment variable (`-passwordEnv`), never on its
command line, so it does not show up in `ps` output for other users on a shared machine.

## Documentation

See [`docs/architecture.md`](docs/architecture.md) and the architecture decision records in
[`docs/adr/`](docs/adr/).

## License

TBD.
