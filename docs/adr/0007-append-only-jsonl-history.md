# 7. Append-only JSONL submission history

- Status: accepted
- Date: 2026-07-23

## Context

The tool needs a durable local record of what it has submitted, so `ena-submit status` can report
past submissions and their accessions without re-querying ENA, and so a submission run is auditable
after the fact. This was recorded ahead of implementation so the surrounding milestones (MAG
submission, chromosome fallback) could assume a stable history contract; the `status` milestone now
implements it.

Options considered for the store:

- **No local state** — re-query ENA each time. Requires credentials just to view history, is slow,
  and cannot record validate-only runs (which mint no accessions).
- **An embedded database** (e.g. SQLite) — queryable, but adds a dependency and a schema-migration
  burden well beyond the tool's needs.
- **An append-only log** — a line per submission attempt, written once and never rewritten.

## Decision

Record submissions as **JSON Lines** in `.ena-submit/history.jsonl`: one JSON object per attempt,
appended, never mutated. Each record carries a `timestamp` (RFC 3339, UTC), the Webin-CLI `context`
(`reads`/`genome`), the object `name`, the `mode` (`validate`/`submit`), the target `environment`
(`test`/`production`), the `outcome` (`success`/`failure`), and optionally `accessions` (each a
`{type, accession}` pair), the `receipt` file path, and an `error` string. `status` reads the file
and renders it; the append is the tool's only write.

## Consequences

- Trivially auditable and greppable; appends never conflict with or rewrite prior lines, so the file
  diffs cleanly and survives interrupted runs.
- No database dependency or migrations; the format is self-describing and forward-compatible (readers
  ignore unknown fields).
- Querying is a linear scan with no indexing and no compaction — fine at the expected scale (tens to
  hundreds of objects). Revisit only if history grows unexpectedly large.
- Concurrent runs could interleave appended lines, but each line is a standalone record, so the file
  stays valid. `.ena-submit/` is already git-ignored.
- Readers ignore unknown fields, so the schema can grow without breaking older builds; a line that
  cannot be parsed at all is surfaced as an error naming the line rather than silently dropped.
