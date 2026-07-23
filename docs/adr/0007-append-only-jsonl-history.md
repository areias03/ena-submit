# 7. Append-only JSONL submission history

- Status: proposed (pending the `status` milestone)
- Date: 2026-07-23

## Context

The tool needs a durable local record of what it has submitted, so `ena-submit status` can report
past submissions and their accessions without re-querying ENA, and so a submission run is auditable
after the fact. This is a forward-looking decision recorded now so the surrounding milestones (MAG
submission, chromosome fallback) can assume a stable history contract; it is not yet implemented.

Options considered for the store:

- **No local state** — re-query ENA each time. Requires credentials just to view history, is slow,
  and cannot record validate-only runs (which mint no accessions).
- **An embedded database** (e.g. SQLite) — queryable, but adds a dependency and a schema-migration
  burden well beyond the tool's needs.
- **An append-only log** — a line per submission attempt, written once and never rewritten.

## Decision

Record submissions as **JSON Lines** in `.ena-submit/history.jsonl`: one JSON object per attempt,
appended, never mutated. Each record carries at least a timestamp, the Webin-CLI context
(`reads`/`genome`), the object name, the mode (`validate`/`submit`), the target environment
(`test`/`production`), any returned accessions, the receipt-file path, and the outcome. `status`
reads the file and renders it; the append is the tool's only write.

## Consequences

- Trivially auditable and greppable; appends never conflict with or rewrite prior lines, so the file
  diffs cleanly and survives interrupted runs.
- No database dependency or migrations; the format is self-describing and forward-compatible (readers
  ignore unknown fields).
- Querying is a linear scan with no indexing and no compaction — fine at the expected scale (tens to
  hundreds of objects). Revisit only if history grows unexpectedly large.
- Concurrent runs could interleave appended lines, but each line is a standalone record, so the file
  stays valid. `.ena-submit/` is already git-ignored.
- Marked **proposed**: field names and the exact schema are fixed when the `status` milestone lands.
