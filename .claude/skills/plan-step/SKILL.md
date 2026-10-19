---
name: plan-step
description: Plan a roadmap step (e.g. M2.3b) - settle its open questions with the user, write its Proposed ADRs and an implementation plan in .claude/plans/, have them reviewed by a fresh plan-reviewer subagent, and stop for the user to accept them.
argument-hint: "<step, e.g. M2.3b>"
disable-model-invocation: true
---

# Plan $ARGUMENTS

You are the planner. You hold the design conversation with the user and
write the documents; a plan-reviewer that was not part of that conversation
reviews them. Talk to the user in their language; everything written to
the repository is in English. If no step was given, plan the one the
roadmap's status names as next.

## 1. Understand the step

Read in `docs/roadmap.md` what the step delivers, the decisions already
agreed for it, the pending decisions it must settle, and its program on
QEMU. Then read the ADRs it builds on and the code it touches. For a broad
search, use the Explore agent and keep only its conclusions.

## 2. Settle the open questions

Design decisions are the user's. Before writing, put every question that
the roadmap and the accepted ADRs leave open to the user, each with the
options and your recommendation.

## 3. Write

- One Proposed ADR per decision, following `docs/adr/README.md` (template,
  number, index). If the step already has Proposed ADRs, revise those.
- The plan, in `.claude/plans/$ARGUMENTS.md`, in the format below.

## 4. Review

Spawn a fresh plan-reviewer and give it only the paths of the ADRs and the
plan, and the step name. Leave out your reasoning, the alternatives you
dropped and your opinion of the design: the reviewer judges the documents
as written, the way a contributor who was not in this conversation will
read them.

## 5. Answer every finding

For each numbered finding, one of:

- **accepted**: fix it in the ADR or the plan;
- **rejected**: write the reason into the ADR, so that the next reader and
  the next review see it;
- **deferred**: add it to the roadmap's pending decisions, under the step
  that will settle it.

A finding that needs a design decision goes to the user first. Then
continue the same reviewer (SendMessage) with the list of answers for a
second pass; if it cannot be continued, spawn a fresh one with the
documents and the answers. After two passes, remaining disagreements go to
the user.

## 6. Stop

Show the user the ADRs and the plan, each finding with its answer, and
what still needs their decision. Do not mark anything Accepted: the user
does. Once the ADRs are accepted, the step is ready for
`/build-step $ARGUMENTS`.

## Plan format

```markdown
# Plan: <step> <title>

ADRs: <numbers>
Program on QEMU: <from the roadmap>
Out of scope: <what this step leaves out, and which step takes it>

## 1. <commit message, e.g. feat(parser): parse static items>
- Status: todo
- Implements: <ADR and section>
- Changes: <layers and files>
- Tests: <which tests, at which layer, and what each proves>

## 2. ...

## N. docs: document <feature> and mark <step> done
- Status: todo
- Changes: docs/reference.md, docs/roadmap.md
```

Each step is one logical commit that builds and passes every test on its
own (CONTRIBUTING.md), in an order where no step needs a later one.

The plan is versioned, and is committed with its accepted ADRs
(`docs(adr): ...`). During `/build-step`, each step gains a
`- Decisions:` list: the choices made while implementing it that the
plan and the ADRs do not dictate, and why.
