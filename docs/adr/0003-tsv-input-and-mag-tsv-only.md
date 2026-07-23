# 3. TSV input, and MAG samples as generated-TSV-only

- Status: partially superseded by [ADR 0004](0004-mag-sample-taxid-fill.md) (the MAG-sample
  decision changed from *generate* to *complete a user-provided sheet*). The TSV-input decision
  still stands.
- Date: 2026-07-23

## Context

Two scoping questions shaped the tool:

1. **How does metadata enter the tool?** Options were a YAML/TOML config, a TSV/CSV spreadsheet, or
   raw CLI flags.
2. **How far should the tool go for MAG samples?** The user creates the study and non-MAG samples
   manually. MAG (metagenome-assembled genome) samples are the exception — each MAG needs a derived
   sample registered against an ENA MAG checklist (ERC000047 bins / ERC000050 MAGs). The tool could
   either just generate the sample TSV, or also register samples via XML (as EBI's `genome_uploader`
   does).

## Decision

1. **Input is TSV/CSV**, one row per run / assembly / MAG bin. This matches how bioinformatics
   pipelines already emit metadata and how ENA users think about submissions, and it version-controls
   cleanly.
2. **MAG samples: generate TSV only.** The tool writes a checklist-conformant `mag_samples.tsv`; the
   user uploads it via the Webin spreadsheet UI and feeds the returned `ERS…` accessions back into
   `mag submit`. The tool does **not** register samples via XML.

## Consequences

- No native XML sample-registration code path — smaller, simpler tool.
- The MAG workflow is an explicit three-step handoff (`prepare` → user uploads → `submit`), which is
  auditable but requires a manual step between the two tool invocations. Documented in the README.
- Input validation lives in one place (`input.rs`) over a stable tabular schema, with per-row error
  reporting.
- Rejected: YAML/TOML config and CLI-flag input; full XML sample registration.
