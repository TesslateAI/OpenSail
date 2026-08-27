# Agent operating contract

Read, in order, before editing:

1. `RELEASE.md`
2. `ARCHITECTURE.md`
3. `DECISIONS.md`
4. `ENGINEERING.md`
5. Release 0 control issue `#6`
6. the assigned packet issue body

## Prime directive

Implement the packet's smallest real result. The packet goal, allowed paths, source authority, and acceptance command are the operating contract.

An explicit allowed path may be created when absent. A required implementation dependency or small private interface may be added when it directly serves the packet. Do not stop merely because the repository did not already contain the destination shape.

## Durable authorities

```text
ARCHITECTURE.md  current accepted system and trust boundaries
DECISIONS.md     rare load-bearing decisions
RELEASE.md       Stage/Checkpoint state, commands, PASS SHAs, active packets
ENGINEERING.md   stable SDLC, test, review, infrastructure, and hygiene rules
issue #6         high-level Release 0 checklist and off-rail guardrail
packet issue     one bounded current work packet
PR               implementation and one material review
```

Do not create a parallel roadmap, design thread, evidence issue, or coordination ledger.

## Write authorization

Questions, explanations, design reviews, code reviews, checks, and requests to explain `why` are read-only.

Repository or GitHub writes require an explicit instruction to create, update, implement, merge, close, delete, or otherwise mutate a named scope. Do not carry write authorization from an earlier task into a later read-only request.

## Work hierarchy

```text
Release -> Stage -> Checkpoint -> Packet
```

A packet is the only unit an implementation agent owns. Waves and rails are informal descriptions, not managed state.

## Packet execution

- Work inside the allowed paths. Creating an explicitly allowed path is authorized.
- Keep `main` runnable and keep the branch directed at the acceptance result.
- Use the real boundary when available; a mock cannot pass a product checkpoint.
- Make the simplest reasonable implementation choice inside the frozen design. Do not escalate ordinary coding decisions.
- Delete abandoned approaches instead of layering compatibility code over them.
- Do not use issue comments for progress logs, claims, evidence dumps, or orchestration.
- The orchestrator owns issue status and release-board updates. The implementation agent supplies a concise final handoff and PR.

## Review signals, not stop triggers

The following are reasons to inspect scope, not reasons to halt work by themselves:

```text
300 or 800 authored lines
4 or 8 changed files
a new directory inside allowed paths
a dependency required by the packet
a small private wire format or helper API
test code exceeding product code for a focused boundary
```

When the change grows, demonstrate the strongest runnable result available and keep reducing toward the packet goal. Explain material size in the PR. Do not stop merely to request permission for repository shape.

## Stop only for a real contradiction

Stop implementation only when continuing requires one of these:

```text
changing a frozen trust boundary, state owner, product resource, language, or checkpoint meaning
adding an unapproved long-lived service, public protocol, scheduler, authority, or persistent store
placing a protected credential in a component that the architecture forbids from holding it
performing an unapproved destructive operation on real data or hardware
changing the accepted Firecracker/Kata security mechanism rather than extracting it
working outside the packet's product scope in a way that cannot be isolated
```

A missing live estate, temporary rescue environment, unavailable external dependency, or unmerged neighboring packet does not cancel safe source work. Complete the independent implementation, record the precise live/integration dependency in the handoff, and continue when it becomes available.

If a real contradiction occurs, report only:

```text
Current decision
Observed contradiction
Concrete source or failed-demonstration evidence
Smallest viable change
Affected checkpoint
Affected paths
```

Do not build a process around the report.

## Source extraction

For `ginit64/*` source repositories named in `docs/provenance/SOURCES.toml`, reuse is explicitly authorized for this project. Absence of a repository-level outbound license is not an internal-copy stop condition. Preserve embedded copyright, SPDX headers, and third-party notices.

For external sources, use the license already recorded in `SOURCES.toml` and preserve its required notice. Do not repeat a license investigation in every packet.

A source-extraction packet records exact source repository, commit, and copied paths in its PR. Byte-preserved imports are distinguished from authored adaptation. Do not bulk-copy a legacy product, its process machinery, or tests that protect only the old repository's internal contracts.

## Tests and review

Add tests only for stable product behavior or a material security, durability, destructive-operation, interoperability, or concurrency invariant.

Do not add tests for non-goals, temporary implementation choices, reviewer preferences, repository history, or the continued absence of future features.

Default review is one material review. After a blocker repair, review the repair and directly affected code. After two failed repair rounds, reduce, split, or replace the implementation instead of layering patches.

## Forbidden agent behavior

Do not:

```text
build an evidence, review, history, or scheduling framework
import complete legacy repositories or preserve their product boundaries
add identity tokens, capabilities, scopes, TTL refresh, or generic grants
add shell provisioning or GitHub Actions
claim a checkpoint from a branch, mock, skipped test, or absent prerequisite
hide a failure behind fallback, retry, inferred success, or invented state
silently redesign architecture or checkpoint meaning
turn a review question into repository mutation
stop for a syntactic guardrail when the packet explicitly authorizes the work
```

## Completion

Packet state is only:

```text
READY -> ACTIVE -> DEMO -> MERGED
                         -> STOPPED
```

Checkpoint state is only:

```text
BLOCKED -> READY -> PASS
```

An implementation agent opens a PR and returns:

```text
goal
demo command and observed result
branch head
changed files
source imports
known limitation
PR
```

The orchestrator updates issue status and records checkpoint `PASS` only after the real acceptance command succeeds on merged `main`.