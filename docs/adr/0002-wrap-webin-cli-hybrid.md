# 2. Hybrid engine: wrap the official Webin-CLI rather than reimplement ENA submission

- Status: accepted
- Date: 2026-07-23

## Context

Actually submitting to ENA requires: schema/content validation against ENA checklists, file
integrity (MD5), and transfer via FTP or Aspera, then parsing a receipt XML for accessions. ENA
ships an official, actively maintained Java tool — **Webin-CLI** (`enasequence/webin-cli`) — that
does all of this for the `reads` and `genome` contexts. A pure-Rust reimplementation would have to
re-derive ENA's validation rules and transfer logic and track them as ENA changes.

## Decision

Use a **hybrid** design. Native Rust owns everything *around* submission — TSV parsing, manifest and
MAG-sample-TSV generation, receipt parsing, config, and history. The **Webin-CLI jar** owns the
submission itself: validation + upload + receipt generation. The Rust `webin` module shells out to
`java -jar webin-cli.jar …`, preflight-checking that Java 17+ and the jar are present.

## Consequences

- We inherit ENA's authoritative validation and transfer for free, and stay correct as ENA evolves.
- Runtime dependency on Java 17+ and the Webin-CLI jar; preflight checks give clear errors when
  absent, and the README documents installation.
- Our surface area (and test burden) shrinks to generation + orchestration + parsing, which are
  exactly the error-prone, manual steps we set out to automate.
- Rejected: **pure-Rust REST/XML** (largest effort, duplicates ENA logic) and a full XML sample
  registration path (see ADR 0003).
