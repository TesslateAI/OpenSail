# Engineering rules

## Default loop

```text
implement
 -> focused test where justified
 -> relevant build/check
 -> runnable demonstration
 -> one material review
 -> merge or discard
```

No CLAIM ledger, evidence bundle, mutation campaign, repeated full-suite campaign, or second review exists by default.

## Progress over form

The packet's explicit goal and allowed paths control implementation. An allowed directory may be created when absent. A necessary dependency, helper, or small private interface may be added when it directly serves the packet.

Line counts, file counts, directory creation, and dependency additions are review signals. They are not automatic stop conditions. Stop only for a real product-boundary, security, destructive-operation, or state-ownership contradiction.

Unavailable live infrastructure does not cancel safe source work. Complete the independent implementation and report the exact live proof still pending.

## Tests

A test must protect a stable externally meaningful contract or a high-cost security, durability, destructive-operation, interoperability, or concurrency invariant.

Do not add a test because:

```text
a PR chose not to implement something
a reviewer mentioned a hypothetical alternative
the current implementation happens to have a shape
a bug was fixed but recurrence is implausible and cheap
```

Assert only the invariant under test. Prefer changing an existing test. Delete tests that protect no important contract or require disproportionate maintenance.

## Review

Findings are:

```text
BLOCKER — concrete correctness, security, data-loss, interoperability, or major maintainability failure
NOTE    — useful improvement, alternative, or speculative hardening
```

After blockers are repaired, review the repair and directly affected code. Do not reopen the complete change for unrelated hardening. After two failed repair rounds, reduce, split, replace, or discard the implementation.

## Scope and simplicity

Prefer the smallest production path. Do not add:

```text
future-provider abstractions
generic policy or permission engines
compatibility layers for abandoned products
second stores or second authorities
frameworks before one real consumer exists
process or evidence tooling unless explicitly required
```

A small internal API or wire shape needed by one real boundary is normal implementation, not a protocol program. Delete obsolete code instead of preserving it ceremonially.

## Source reuse

`ginit64/*` sources listed in `docs/provenance/SOURCES.toml` are explicitly authorized internal quarries for this project. Preserve embedded copyright, SPDX headers, and third-party notices. Do not block implementation on the absence of a repository-level outbound license.

External source licenses are recorded once in `SOURCES.toml`; packets preserve the stated notice and do not repeat the review.

## Infrastructure

Production provisioning must not use project-authored shell state machines.

Prohibited:

```text
GitHub Actions
OpenTofu local-exec, remote-exec, or executor provisioners
Ansible raw, script, shell, or command-through-sh provisioning
remote bash heredocs and ad-hoc SSH command strings
Nix activation scripts used as cluster provisioning logic
mutable package installation outside Nix
```

Direct, bounded operational commands used for development or a live checkpoint are allowed. The prohibition is against shell as the provisioning control plane, not against invoking a real tool.

A missing structured module may be implemented as a small typed idempotent utility with structured output when the product actually needs it.

## Repository and secrets

Git must not contain:

```text
real IPs, domains, tenant/subscription/resource IDs, host keys, machine IDs, or disk identities
production inventory, host_vars, group_vars, .tfvars, .env, or local generated state
secret values or encrypted secret values
```

Nix may reference a runtime credential path but must never evaluate or build secret bytes into the Nix store.

## Baseline command

```bash
nix develop --command just check
```

Run focused checks during implementation and the baseline before PR. Add checks only when a stable repository contract justifies them.