# 0001. Record architecture decisions

- Status: Accepted
- Date: 2026-10-12

## Context

Erebos is designed largely through conversations (with people and with AI
agents). That context is lost when a chat ends, and decisions made without
written rationale tend to be relitigated or silently broken.

## Decision

Significant decisions are recorded as ADRs in `docs/adr/`, following the
process in [`README.md`](README.md). ADRs are written in English.

## Consequences

- Every non-trivial design change carries a small documentation cost.
- Agents and new contributors can recover the "why" from the repository alone.
- Accepted ADRs are never edited in substance; they are superseded.
