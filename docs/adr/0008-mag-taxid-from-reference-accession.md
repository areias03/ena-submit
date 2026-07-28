# 8. Resolve MAG tax ids from the GTDB-Tk reference accession, not from names

Date: 2026-07-28

## Status

Accepted. Supersedes the name-resolution part of
[ADR 0004](0004-mag-sample-taxid-fill.md); the rest of 0004 (complete the sheet rather than
generate it, generic `Table`, opaque pass-through columns) still holds.

## Context

ADR 0004 resolved each row by looking its GTDB name up in the ENA taxonomy API. Measured on the
2676-row reference sheet, that fails on **507 rows (19%)**, from 58 distinct names, for two reasons
that no amount of better name handling can fix:

- **GTDB-only names.** GTDB's placeholder genera — `CAG-269`, `UBA9414`, `JAGZHZ01`,
  `MGBC140090`, `GCA-900066495` — are GTDB identifiers. They exist in no ENA record, under any
  spelling.
- **Names ENA has not adopted.** GTDB and NCBI disagree about current nomenclature:
  `Ruminococcus gnavus` is *Mediterraneibacter* in NCBI, `Clostridium innocuum` is
  *Thomasclavelia*, `Prevotella copri` is *Segatella*. ENA returns nothing for the GTDB spelling.
  Even `Rothia sp.` — an ordinary genus — has no ENA record in that form.

Names are the wrong key. But GTDB-Tk already records a better one for every bin: the accession of
the reference genome it matched (`fastani_reference`, e.g. `GCA_900553985.1`), which the sheet
carries as `GTDBtk fastani Ref`. An accession is stable, unambiguous, and maps directly into NCBI
taxonomy — which is the taxonomy ENA shares.

## Decision

**Resolve each row from its reference genome accession**, in two hops:

1. **Accession → NCBI species taxon id**, via the NCBI Datasets v2 API (`crate::ncbi`).
2. **Taxon id → ENA name + submittability**, via the ENA taxonomy API's `tax-id/{id}` endpoint.

ENA stays in the loop and cannot be dropped: the `tax_id` column must hold a taxon *ENA* accepts,
and `scientific_name` must be ENA's exact name for that id. What changes is that ENA is now queried
**by id** — an exact lookup that cannot be ambiguous and cannot miss a synonym — rather than by
name.

### Why NCBI Datasets rather than the GTDB API

The GTDB API is the obvious first choice, and it does work: `/genome/{accession}/card` returns
`ncbi_species_taxid` directly, saving the rank climb below. It was rejected because
**`api.gtdb.ecogenomic.org` serves a Cloudflare Origin certificate** — a certificate valid only for
Cloudflare-to-origin traffic, chained to no publicly trusted root. This is not a local artefact:
Google and Cloudflare public DNS both resolve the host straight to the origin (203.101.231.56), and
port 80 serves no API. Any correctly configured HTTPS client, `ureq` included, rejects it. The
workaround — bundling the Cloudflare Origin CA as a trust anchor — would ship a root that signs
origin certificates for every Cloudflare customer, i.e. that authenticates essentially nothing.

NCBI Datasets returns the same taxon ids over ordinary trusted TLS (verified case by case), and
**batches**: `POST /genome/dataset_report` takes 100 accessions and `POST /taxonomy` takes 200 ids,
so a whole sheet costs about five requests rather than one per accession. GTDB's answer is still
what we use — GTDB-Tk chose the reference genome — only the lookup route changes.

### Climbing to species rank

A genome's own taxon is often not a species. Of the reference sheet's 172 distinct genome taxa,
**51 are `STRAIN` and 3 `SUBSPECIES`**; `GCF_` (RefSeq isolate) accessions are 122 of 224. A MAG is
not the type strain it happens to resemble, so submitting `GCF_000012825.1` as *Phocaeicola
vulgatus ATCC 8482* (435590) would be wrong — it must be the species, 821. Non-species taxa are
therefore climbed: one extra batched `/taxonomy` call fetches the ranks of every ancestor, and the
nearest `SPECIES` ancestor wins. All 172 climbed successfully.

### Fallback for rows with no reference

GTDB-Tk writes `0` in the reference column when it made no species assignment — 470 rows here — and
a handful of accessions (4) name assemblies NCBI has since suppressed or replaced. Those rows fall
back to ENA **name** lookups built from the GTDB lineage, deepest rank first: the species,
`"{genus} sp."`, `"uncultured {genus} sp."`, then `"{family} bacterium"` and on up through order,
class and phylum. `"{rank} bacterium"` is how NCBI names taxa for bins identified no further than a
family, and it is what rescues the GTDB-only genera. The domain is never tried — `Bacteria
bacterium` names nothing. A GTDB placeholder species (`sp<digits>`) is skipped rather than tried, as
in ADR 0004: it provably matches nothing.

The two triggers are not equally benign, so they are not reported alike. A `0` cell is expected —
GTDB-Tk matched nothing, and there was never an accession to use. An accession NCBI cannot resolve
means the *sheet* is stale, pointing at an assembly that no longer exists, and is worth refreshing
from a newer GTDB release. The run still completes either way, but the latter emits a `WARN` naming
the accession and how many rows it affects (once per accession, not per row) rather than passing
silently.

`GTDBtk fastani Ref` joins `scientific_name` and `tax_id` as a required **column**; an empty or `0`
**cell** is normal and simply routes the row to the fallback.

## Consequences

- **The sheet resolves completely.** 2676/2676 rows, every one submittable in ENA: 2152 from the
  accession (167 distinct species taxa, all `submittable: true`), 524 from the fallback chain. The
  previous approach left 507 rows failing, and since row problems are aggregated into one error,
  that meant the sheet could not be prepared at all.
- **Names in the sheet no longer decide anything on the primary path.** `scientific_name` is
  overwritten with ENA's name for the resolved id; the GTDB classification is read only for the
  fallback. Rows that already carry a `tax_id` are still skipped entirely, so re-runs stay no-ops.
- **A second network dependency** (NCBI Datasets), scoped like the first to `mag prepare`. It is
  batched and unauthenticated; HTTP 429 is retried with a short backoff rather than failing the run.
- **A new required column.** Sheets predating this change need `GTDBtk fastani Ref` added; the
  error names it explicitly. `init`'s template carries it.
- Request count drops rather than rises: ~5 batched NCBI POSTs plus 272 memoized ENA lookups,
  against 216 ENA lookups before, for a run of comparable length (~105 s).
