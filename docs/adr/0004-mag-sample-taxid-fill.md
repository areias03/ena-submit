# 4. MAG samples: complete the user's sheet by filling `tax_id`

- Status: accepted, with two parts superseded by
  [ADR 0008](0008-mag-taxid-from-reference-accession.md):
  - **how the taxon id is resolved** — from the row's GTDB-Tk reference genome accession, not by
    matching its name against ENA. The name-matching described below survives only as ADR 0008's
    fallback for rows GTDB-Tk matched no reference for;
  - **the required-column set** — `GTDBtk fastani Ref` joins `scientific_name` and `tax_id`.

  Everything else here still holds: `mag prepare` *completes* the user's sheet rather than
  generating one, reads it with the generic `Table`, and passes every other column through
  untouched.
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

The retry costs a second request for every falling-back row, which made the request count the
dominant cost on a full sheet (4282 requests, ~80 min). This is now solved by memoizing lookups by
name — see the update in [ADR 0005](0005-blocking-http-ureq.md); a full sheet issues 268 requests
and completes in ~77 s. A GTDB placeholder also skips the direct lookup entirely, since a
`sp<digits>` epithet provably matches nothing in ENA (verified: the endpoint returns an empty list);
its error message says so rather than implying an attempt that never happened. A bare genus keeps
the direct-first order, because it *does* resolve — to a non-submittable taxon — and that is
information worth having.

## Update (2026-07-28): the sheet carries GTDB-Tk classifications

The `scientific_name` column now holds the **GTDB-Tk classification string verbatim** —
`d__Bacteria;…;g__Phocaeicola;s__Phocaeicola vulgatus` — rather than a name someone reduced by
hand beforehand. That reduction was the one manual step left in front of `mag prepare`, and it is
exactly the step the previous update's fallback logic was already half-doing; folding it in makes
the tool consume GTDB-Tk's output directly.

A new `gtdb` module parses a classification into the deepest rank that names a taxon: the species
if GTDB assigned one, otherwise the genus. Everything downstream is unchanged — the parsed name
goes through the same direct-lookup / `"{genus} sp."` retry described above.

Two details the parser has to get right:

- **Polyphyly suffixes are stripped.** GTDB writes `Clostridium_AQ`, `Bacteroides fragilis_A`,
  `Anaerobiospirillum_A thomasii`; the suffix marks a GTDB split of a name NCBI keeps whole, and
  appears in no ENA record. The previous update handled the *spaced* form (`Clostridium AQ
  sp000165065`) because the hand-reduced sheets had substituted spaces for the underscores; raw
  GTDB output has the underscore, which `is_genus_token`'s all-ASCII-letters check would reject
  outright. Stripping is per whitespace token, since the suffix sits on either half of a binomial.
- **A genus-only lineage skips the direct lookup.** When `s__` is empty the rank is *known* from
  the lineage, so the bare-genus direct-first order no longer buys information — the genus is a
  real but non-submittable taxon, and the fallback is the only outcome. This is the same
  doomed-request reasoning that already applies to `sp<digits>` placeholders. A single-token
  species field still takes the direct-first path.

Consequences:

- **`scientific_name` is now rewritten on nearly every row**, not just fallback rows, since a
  classification string is never a valid ENA name. The "only `tax_id` is written" rule from the
  original decision is fully retired; the command reports the rewrite count and how many went via
  the `"<genus> sp."` form.
- **A plain scientific name is no longer accepted as input** — it is reported per row as "not a
  GTDB classification". Re-running on an output sheet is still a no-op, because those rows carry a
  `tax_id` and are skipped before the cell is ever parsed.
- A lineage empty at both `g__` and `s__` (4 rows of the 2676-row reference sheet) is a row-level
  problem naming its deepest populated rank, so it can be fixed by hand. As with every other row
  problem, one such row fails the whole run.
