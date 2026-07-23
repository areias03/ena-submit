# 4. MAG samples: complete the user's sheet by filling `tax_id`

- Status: accepted
- Date: 2026-07-23
- Supersedes the MAG-sample half of [ADR 0003](0003-tsv-input-and-mag-tsv-only.md)

## Context

ADR 0003 assumed the tool would *generate* the MAG sample sheet from per-bin metadata. In practice
the user already produces a near-complete MAG sample TSV (the ENA checklist spreadsheet) from their
pipeline — every column is filled **except `tax_id`**, which requires resolving each organism to an
NCBI/ENA taxon id. Regenerating the sheet would mean re-modelling every checklist column the tool
doesn't care about and risking loss of the user's data.

## Decision

`ena-submit mag prepare` **completes** the user-provided sheet rather than generating one:

- The sheet is read with the **generic `input::Table`** reader, preserving every column and its order.
- For each row, the tool resolves the value in the **`scientific_name`** column to a taxon id via the
  **ENA taxonomy REST API** and writes it into the **`tax_id`** column; all other cells pass through
  untouched.
- Output is a completed TSV the user uploads via the Webin spreadsheet UI to obtain `ERS…` accessions.

MAG **assembly** submission is unchanged in spirit: it reuses [`AssemblyRecord`] with
`assembly_type = "Metagenome-Assembled Genome (MAG)"` and `sample` taken from the
`registered_mags.tsv` (`bin_name → ERS…`) mapping.

## Consequences

- No rigid model of the MAG checklist — the tool only needs `scientific_name` and `tax_id` to exist;
  everything else is opaque pass-through, so checklist changes don't break us.
- Adds a **network dependency** (ENA taxonomy API) and an HTTP client, scoped to `mag prepare`.
  Ambiguous/unsubmittable names must be surfaced as errors.
- The three-step MAG handoff (`prepare` → upload → `submit`) from ADR 0003 still holds; only the
  content of step one changed (fill vs generate).
- `input.rs` gains a generic order-preserving `Table` type alongside the typed reads/assembly readers.
