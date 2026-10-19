---
name: build-step
description: Implement a planned roadmap step from .claude/plans/<step>.md, one commit per plan step - the implementer subagent writes it, a fresh code-reviewer subagent reviews it, and it is committed only after the review approves and the gates pass.
argument-hint: "<step, e.g. M2.3b>"
disable-model-invocation: true
---

# Build $ARGUMENTS

You orchestrate; you do not write or review the code yourself. The
implementer writes each step, a code-reviewer that has not seen the
implementer's reasoning reviews it, and you route work between them, keep
the plan's progress and commit. A change you made yourself would reach a
commit unreviewed. Talk to the user in their language.

Running this command is the user's request to commit each plan step once
its review approves it. Commit nothing else, and never push.

## Before the first step

1. Read `.claude/plans/$ARGUMENTS.md`. If it does not exist, stop and point
   the user to `/plan-step $ARGUMENTS`.
2. Every ADR the plan names must be Accepted. If one is still Proposed,
   stop: accepting it is the user's decision.
3. Run `git status`. If anything is uncommitted, list it and ask the user
   what to do with it (for example, commit the accepted ADRs, with the
   plan, first as `docs(adr): ...`). Each step starts from a clean tree, so everything
   uncommitted after it belongs to it.
4. If the plan has no `Base:` line, add one with the current `HEAD`. Start
   at the first step whose status is not `done`.

## Each step

1. **Implement.** Give the implementer the plan path and the step number.
   Spawn a fresh implementer for each step, and continue it with
   SendMessage only for that step's fixes. One implementer kept across
   steps rereads its whole history on every call, which grows with each
   step; the plan and the code carry what the next step needs. If the
   tree holds an unfinished diff of the step (an interrupted run), tell
   the new implementer so, and that it is responsible for the whole diff.
2. **Read its report.** If it stopped on a question, or deviated from the
   plan in a way that touches a decision, bring it to the user. You do not
   settle design questions. Check that the step has its `Decisions` list
   in the plan; if it is missing, send the implementer back to write it.
3. **Review.** Spawn a fresh code-reviewer for each step. Give it the plan
   path and the step number, and say the change is the uncommitted diff.
   Do not pass on the implementer's report or your own view of the change.
4. **Fix.** Send the blocking and should-fix findings to the implementer
   with the reviewer's numbers, then continue the same reviewer with what
   was fixed, for a re-check. If the implementer disputes a finding, put
   both positions to the user. Keep minor findings for the final report.
   After two rounds with findings still open, stop and ask the user.
5. **Commit.** Once the reviewer approves, set the step's status to
   `done` in the plan and commit the step with the plan's commit message
   (Conventional Commits, CONTRIBUTING.md). The plan is versioned: its
   changes for the step (status, `Decisions`) go in the step's commit.

## After the last step

Spawn a fresh code-reviewer over the range from `Base` to `HEAD`, against
the whole plan: every rule of its ADRs implemented and tested, the docs
updated, the program on QEMU in place, every step's `Decisions` present.
Route its findings as above; fixes become new commits. Then write the
minor findings left open under `## Left open` at the end of the plan and
commit it (`docs: record what $ARGUMENTS left open`), so they are not
lost with this conversation. Report to the user: the commits, the minor
findings left open, and anything deferred to a later step.
