# 1. Record architecture decisions

- Status: accepted
- Date: 2026-07-23

## Context

We want a durable, reviewable history of the significant technical decisions behind `ena-submit`,
so future contributors (and our future selves) understand *why* the tool is shaped the way it is.

## Decision

We will use Architecture Decision Records (ADRs), one Markdown file per decision in `docs/adr/`,
numbered sequentially, following Michael Nygard's format. Superseded decisions are kept and marked,
never deleted. The living development plan is tracked separately; ADRs capture the *why* of settled
choices.

## Consequences

- Every non-trivial architectural choice gets a short, self-contained record.
- Reviewers can see the trade-offs without archaeology through commits or chat logs.
- Slight overhead per decision, accepted as the cost of an auditable project history.
