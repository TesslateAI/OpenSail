# Release 0 control board

This file is the exact Stage/Checkpoint authority. It records acceptance commands and PASS SHAs. A checkpoint passes only on a real command run against the integrated product; no PASS is recorded before it is exercised.

Implementation controls are outcome-based: explicit packet paths may be created, necessary dependencies/private interfaces may be added, and line/file counts are review signals rather than stop conditions. Only a real product-boundary, security, destructive-operation, or state-ownership contradiction stops work.

## Release contract

Release 0 ships this exact path:

```text
Native-or-optional external login
 -> internal User
 -> Personal or Team scope
 -> Workspace
 -> New chat: the first message atomically creates Conversation/Session + Run
 -> live model and tool interaction through voie-fabricd into a jailed Firecracker Workspace
 -> durable queued follow-ups while a Run is active
 -> refresh reconstructs the same conversation
```

Mandatory proofs:

```text
foreign-project access is denied
Workspace marker survives Firecracker execution replacement
ambiguous exec becomes outcome-unknown and is not replayed
no command executes on the control or activation host
Firecracker guest receives no protected credential
control and fabric restart paths are recoverable
supported blank-host deployment is non-interactive
cleanup leaves no owned runtime residue
```

## Production Profile 0

```text
one NixOS control VM
one Rust voie-cloud product process
Azure PostgreSQL, Blob, and Key Vault
Headscale transport
product mTLS certificate identifying the exact fabric_id
one enrolled x86_64 KVM Fabric with an automation-capable NixOS bootstrap path
NixOS -> K3s -> Cilium -> patched Kata runtime-rs -> jailed Firecracker
one fixed guest/runtime profile
local block-backed durable Workspaces
one active control instance; no automatic failover
one same-origin native VOIE Console served by voie-cloud
```

The Fabric provider is not part of the runtime contract. A local KVM host and an Azure-hosted host run the identical product code path; they differ only in deployment inputs (see D014).

## Stages

A stage is PASS only when every required checkpoint is PASS. Until then it is OPEN: implementation may proceed in the recorded order, but no stage completion is claimed without checkpoint evidence.

| Stage | Goal | Required checkpoints | State | Exit SHA |
|---|---|---|---|---|
| S0 | Freeze and bootstrap | C0 | PASS | `901205e17b298d0d7c74b038843c7e35e7e1290f` |
| S1 | Working backend vertical | C1–C4 | OPEN | — |
| S2 | Working product surface | C5–C6 | OPEN | — |
| S3 | Deploy and qualify | C7–C8 | OPEN | — |

## Checkpoints

A checkpoint passes only on merged `main`, with the real command and an exact commit SHA. The exact SHA in this file is the stage marker; no duplicate stage tag is required. Every live proof below remains BLOCKED until actually exercised; none carries a PASS SHA today.

| ID | Proof | Acceptance command | State | PASS SHA |
|---|---|---|---|---|
| C0 | Frozen release, executable repository baseline | `nix develop --command just check` | PASS | `901205e17b298d0d7c74b038843c7e35e7e1290f` |
| C1 | Direct Firecracker guest executes through `voie-runner` | `just live-c1` | BLOCKED | — |
| C2 | Workspace marker survives execution E1 -> E2 | `just live-c2` | BLOCKED | — |
| C3 | `voie-cloud` controls the real Fabric | `just live-c3` | BLOCKED | — |
| C4 | Disposable activation performs model -> remote Bash | `just live-c4` | BLOCKED | — |
| C5 | Fresh activation resumes the same Session and Workspace | `just live-c5` | BLOCKED | — |
| C6 | Native VOIE Console performs login -> prompt -> tool -> answer | `just live-c6` | BLOCKED | — |
| C7 | OpenTofu/NixOS/Ansible deployment reproduces C6 | `just live-c7` | BLOCKED | — |
| C8 | Isolation, unknown/no-replay, recovery, restore, and cleanup pass | `just live-c8` | BLOCKED | — |

Commands C1–C8 are reserved contracts: the recipe does not exist until the owning workstream adds its real implementation, and a checkpoint becomes READY only then.

## Implementation order

Workstreams proceed in this order. Each names its owning paths and the checkpoints it must prove. Status lives only in this table via checkpoint states above; no progress-comment stream exists.

| Order | Workstream | Owning paths | Proves |
|---|---|---|---|
| 1 | Control state kernel: PostgreSQL schema, Sessions/Runs/events, writer fencing, health | `crates/voie-cloud/**` | prepares C3, C4 |
| 2 | Fabric runtime: reservations, Firecracker realization, runner shell, runtime Nix | `crates/voie-fabricd/**`, `crates/voie-runner/**` | C1, C2 |
| 3 | Control-fabric integration: `voie-cloud` drives the enrolled Fabric | `crates/voie-cloud/**`, `crates/voie-fabricd/**` | C3 |
| 4 | Activation bridge: inherited connection, model -> remote Bash, session resume | `activation/**`, narrow bridge in `crates/voie-cloud/**` | C4, C5 |
| 5 | Native VOIE Console: same-origin login -> prompt -> tool -> answer | `web/**`, serving surface in `crates/voie-cloud/**` | C6 |
| 6 | Deploy and qualify: estate provisioning, recovery, no-replay, cleanup | deployment inputs per D007 | C7, C8 |

## C0 exit

- [x] Release 0 golden path and non-goals are frozen.
- [x] Architecture and state ownership are frozen.
- [x] Identity-token-capability machinery is explicitly excluded.
- [x] Native VOIE Console decision is recorded; upstream sources and their boundaries are pinned in `docs/provenance/SOURCES.toml`.
- [x] Minimal Rust and TypeScript repository structure exists (`crates/voie-cloud`, `crates/voie-fabricd`, `crates/voie-runner`, `activation`, `web`).
- [x] Console and activation dependency islands are separate.
- [x] No legacy product code is imported.
- [x] No workflow, deployment script, inventory, `.tfvars`, or encrypted secret is present.
- [x] The reviewed bootstrap PR #5 is merged to `main`.
- [x] `nix develop --command just check` passed on merged `main` at `901205e17b298d0d7c74b038843c7e35e7e1290f`.
- [x] C0 records that exact PASS SHA and S0 is closed.

## Non-goals

```text
Go control plane
voie-next authority core or wrapper
principal spine, grant graph, capability token, TTL, or refresh loop
BETTERDAM control plane, Admission, Workload Access, or browser management panel
Whaled resident service, supervisor, CLI, or product protocol
browser-pasted bearer or infrastructure credential gate
Azure Container Apps or Azure Sandbox
multi-fabric scheduler or automatic placement
multi-node K3s, distributed Workspace storage, or live migration
arbitrary guest images
private Git credential broker
PTY, browser terminal, or background process control
generic workload, provider, policy, or service-broker framework
active-active or multi-region control
provisioning shell framework
GitHub Actions
```

Feature work stops after C6. S3 repairs only release-gate blockers.
