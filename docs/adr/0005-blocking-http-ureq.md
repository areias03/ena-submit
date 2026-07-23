# 5. Blocking HTTP via `ureq` for ENA REST calls

- Status: accepted
- Date: 2026-07-23

## Context

`mag prepare` (see [ADR 0004](0004-mag-sample-taxid-fill.md)) introduced the tool's first — and so
far only — network dependency: resolving each `scientific_name` to a taxon id via the ENA taxonomy
REST API. This decision was made during that milestone but never recorded, so it is captured here
retroactively.

The rest of the tool is synchronous and single-threaded, with no async runtime. The HTTP calls it
makes are straight-line, sequential (one taxonomy lookup per sheet row). The realistic client
choices were:

- **`reqwest`** — the de-facto standard, but async-first and pulls in `tokio`; its blocking mode
  still drags a large dependency tree and a runtime we otherwise don't need.
- A **lightweight blocking client** (`ureq`, `attohttpc`, `minreq`) — synchronous, small, no runtime.

## Decision

Use **`ureq`** with its `json` feature and default rustls TLS. Blocking I/O matches the tool's
synchronous control flow: the taxonomy lookups happen in ordinary sequential code, so there is
nothing for an async runtime to overlap. `ureq` keeps the dependency and compile-time footprint
small and, via rustls + bundled webpki roots, needs no system OpenSSL.

## Consequences

- Much smaller dependency tree and build than `reqwest` + `tokio`; synchronous code stays synchronous.
- TLS works out of the box with no system crypto library, easing portability.
- Requests are issued **sequentially**, one per row. Acceptable for the small MAG sample sheets in
  scope; if a future feature needs high-volume concurrent requests, revisit the client choice.
- Sets the precedent that ENA REST calls are blocking; the submission path shells out to Webin-CLI
  ([ADR 0002](0002-wrap-webin-cli-hybrid.md)) rather than making HTTP calls itself.
