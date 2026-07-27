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

## Update (2026-07-27): `"{genus} sp."` fallback

Real sheets are GTDB-derived, and two name shapes in them can never be submitted as written:

- a **bare genus** (`Bacteroides`) — ENA has the taxon but flags it `submittable: "false"`;
- a **GTDB placeholder binomial** (`Phocaeicola sp900556845`), whose epithet is a GTDB accession
  that exists in no ENA record.

Together these are the majority of the reference sheet (470 bare genera and 1135 placeholders out
of 2675 rows), so the "surfaced as errors" rule above made `mag prepare` unusable on real input,
even though the name ENA wants — `{genus} sp.` — is a one-step derivation in both cases.

`mag prepare` now retries such a name as `"{genus} sp."` after, and only after, the direct lookup
fails, so a name ENA does accept is never second-guessed. The genus is the first whitespace token;
the fallback applies when that token is genus-shaped (≥2 ASCII letters, initial capital) **and**
the name is either that token alone or ends in a `sp<digits>` epithet. Taking the first token also
handles GTDB's spaced genus suffixes (`Clostridium AQ sp000165065` → `Clostridium sp.`). Real
binomials are left alone and still error.

On success it writes **both** the `tax_id` and the `scientific_name` cell, a deliberate exception
to the "only `tax_id` is written" decision above: ENA validates the name against the taxon id, so a
sheet carrying `Bacteroides` beside the `Bacteroides sp.` taxId would be rejected downstream. The
rewrite count is reported on stdout.

The retry costs a second request for every falling-back row, and the sequential-request assumption
in [ADR 0005](0005-blocking-http-ureq.md) is now the dominant cost on a full sheet (4282 requests
for 2676 rows, ~80 min). Reducing that is tracked separately.
