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
- `mag prepare` — complete a MAG sample sheet by resolving each row's GTDB-Tk reference genome
  accession to a `tax_id` (NCBI Datasets → ENA) and rewriting `scientific_name` to the matching ENA
  name, falling back to a walk up the GTDB lineage for rows GTDB-Tk matched no reference for.
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

1. `mag prepare <mags.tsv> -o mag_samples.filled.tsv` — fills the `tax_id` column and rewrites
   `scientific_name`; all other checklist columns pass through unchanged.

   Two columns are copied straight out of GTDB-Tk's summary and drive the whole step:

   | column | GTDB-Tk field | example |
   | --- | --- | --- |
   | `scientific_name` | `classification` | `d__Bacteria;…;g__Phocaeicola;s__Phocaeicola vulgatus` |
   | `GTDBtk fastani Ref` | `fastani_reference` | `GCF_000012825.1` |

   The **accession is the key**. `mag prepare` maps it to an NCBI species taxon id (NCBI Datasets
   API), confirms that id against ENA, and writes both the `tax_id` and ENA's own name for it —
   ENA validates `scientific_name` against `tax_id`, so the two have to agree. Matching GTDB's
   *names* against ENA instead fails on roughly a fifth of a real sheet, because many exist only in
   GTDB (`CAG-269`, `UBA9414`) or have since been renamed (`Prevotella copri` → *Segatella copri*).

   | `GTDBtk fastani Ref` | GTDB calls it | ENA name written |
   | --- | --- | --- |
   | `GCF_000012825.1` | `Phocaeicola vulgatus` | `Phocaeicola vulgatus` (821) |
   | `GCF_002224675.1` | `Prevotella copri_A` | `Segatella copri` (165179) |
   | `GCA_900553985.1` | `CAG-269 sp900553985` | `uncultured Clostridium sp.` (59620) |
   | `GCA_018365895.1` | `UBA9414 sp018365895` | `Lachnospiraceae bacterium` (1898203) |

   A reference genome that is itself a *strain* (common for `GCF_` records) is climbed to its
   species first: a MAG is not the type strain it happens to resemble.

   **Rows with no usable accession** — GTDB-Tk writes `0` when it made no species assignment —
   fall back to looking names up in ENA, walking the lineage down until one resolves: the species,
   then `"<genus> sp."`, then `"uncultured <genus> sp."`, then `"<family> bacterium"` and on up
   through order, class and phylum. GTDB's polyphyly suffixes (`_A`, `_AQ`) are stripped for these
   name lookups, since they exist nowhere in ENA.

   | classification | resolved by name to |
   | --- | --- |
   | `g__Rothia;s__` | `uncultured Rothia sp.` |
   | `g__Merdisoma;s__` (GTDB-only genus) | `Lachnospiraceae bacterium` |
   | `f__Eggerthellaceae;g__;s__` | `Eggerthellaceae bacterium` |

   `mag prepare` reports how many cells it rewrote and how many took the fallback. A row whose cell
   is not a classification, or that nothing resolves, is reported with its row number; all such rows
   are collected and reported together. Rows that already carry a `tax_id` are left untouched, so
   re-running on an output sheet is a no-op.
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
