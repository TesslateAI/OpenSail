# Release 0 control board

This file is the exact Stage/Checkpoint authority. It records acceptance commands and checkpoint state. A checkpoint passes only on a real command run against the integrated product; no PASS is recorded before it is exercised.

Acceptance belongs to the working branch or PR revision under test, not to a branch name or merge state. Merge/promotion is a separate repository action and is never a prerequisite for validation or PASS. If promotion changes the effective tree, validate the material delta introduced by that change; do not rerun solely because Git created a merge commit.

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
configured operator management SSH remains usable across required restart/reboot
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

Configured operator management SSH is an intentional persistent operations/development path. C8 must not close it, clear its configured management CIDRs, or treat reachability from an allowed management CIDR as a finding, limitation, or incomplete checkpoint. Changing operator SSH exposure is a separate explicit deployment configuration action.

## Stages

A stage is PASS only when every required checkpoint is PASS. Until then it is OPEN: implementation may proceed in the recorded order, but no stage completion is claimed without checkpoint evidence.

| Stage | Goal | Required checkpoints | State |
|---|---|---|---|
| S0 | Freeze and bootstrap | C0 | PASS |
| S1 | Working backend vertical | C1–C4 | OPEN |
| S2 | Working product surface | C5–C6 | OPEN |
| S3 | Deploy and qualify | C7–C8 | OPEN |

## Checkpoints

A checkpoint passes when its real acceptance command succeeds against the working revision under test. Record PASS immediately on that branch or PR; do not wait for merge. Every live proof below remains BLOCKED until actually exercised.

| ID | Proof | Acceptance command | State |
|---|---|---|---|
| C0 | Frozen release, executable repository baseline | `nix develop --command just check` | PASS |
| C1 | Direct Firecracker guest executes through `voie-runner` | `just live-c1` | BLOCKED |
| C2 | Workspace marker survives execution E1 -> E2 | `just live-c2` | BLOCKED |
| C3 | `voie-cloud` controls the real Fabric | `just live-c3` | BLOCKED |
| C4 | Disposable activation performs model -> remote Bash | `just live-c4` | BLOCKED |
| C5 | Fresh activation resumes the same Session and Workspace | `just live-c5` | BLOCKED |
| C6 | Native VOIE Console performs login -> prompt -> tool -> answer | `just live-c6` | BLOCKED |
| C7 | OpenTofu/NixOS/Ansible deployment reproduces C6 | `just live-c7` | BLOCKED |
| C8 | Isolation, unknown/no-replay, recovery, restore, cleanup, and configured operator management survive reboot | `just live-c8` | BLOCKED |

Commands C1–C8 are reserved contracts: the recipe does not exist until the owning workstream adds its real implementation, and a checkpoint becomes READY only then. Once the real recipe passes, record PASS on that working revision; do not wait for merge.

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
- [x] `nix develop --command just check` passed for the bootstrap revision.
- [x] C0 is PASS and S0 is closed.

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

## Application Platform Profile 1

Profile 1 extends the integrated product. It does not reopen Release 0 checkpoints and does not replace Profile 0 boundaries.

### Profile 1 contract

```text
Existing Project
  └── Application
        ├── one mutable Workspace
        ├── immutable Releases
        ├── private-by-default dev Environment at <slug>.dev.<console-host>
        └── explicit production publication of the exact preview Release
              at <slug>.prod.<console-host>
```

Workspace is where the agent changes code. Release is what the platform trusts as immutable input. Deployment is what the platform supervises.

Primary proof prompt:

```text
Build a private task tracker with PostgreSQL, test it, give me a preview, then publish it to production.
```

### Profile 1 stages

A stage is PASS only when every required checkpoint is PASS on a fully validated working revision with the real command. The revision does not need to be merged.

| Stage | Goal | Required checkpoints | State |
|---|---|---|---|
| P1-S1 | Application and real development image | P1-C1 | OPEN |
| P1-S2 | Immutable Release and private dev preview | P1-C2 | OPEN |
| P1-S3 | PostgreSQL and Environment secrets | P1-C3 | OPEN |
| P1-S4 | Production publication | P1-C4 | OPEN |
| P1-S5 | Hardening and product completion | P1-C5 | OPEN |

### Profile 1 checkpoints

| ID | Proof | Acceptance command | State |
|---|---|---|---|
| P1-C1 | Agent creates an Application, writes and tests a normal web project in the Workspace guest; no project command runs on control or Fabric host | `just live-p1-c1` | BLOCKED |
| P1-C2 | One Release is packaged; private dev URL requires authentication; Workspace mutation does not change the active preview | `just live-p1-c2` | BLOCKED |
| P1-C3 | Dev and prod databases are distinct; Application persists across Pod restart; prod credential never enters Workspace or conversation log | `just live-p1-c3` | BLOCKED |
| P1-C4 | Prod artifact hash equals preview hash; unhealthy candidate receives no production traffic; rollback restores the previous Release | `just live-p1-c4` | BLOCKED |
| P1-C5 | Unknown build, migration, or deployment effects are not replayed; deletion removes routes, Pods, Services, volumes, bindings, and Fabric journal rows | `just live-p1-c5` | BLOCKED |

Commands P1-C1–P1-C5 are reserved contracts. Source work may proceed; a checkpoint becomes READY only when its live recipe exists, and PASS when that recipe succeeds against the working branch or PR revision.

### Profile 1 implementation order

| Order | Workstream | Owning paths | Proves |
|---|---|---|---|
| 1 | Application schema and APIs, slug, fixed Environments, `voie.toml`, agent Application tools, `voie-workspace:v1` | `crates/voie-cloud/**`, `nix/runtime/**`, `activation/**` | P1-C1 |
| 2 | `voie-pack`, Release Blob, `voie-app:v1`, Deployment realization, Fabric gateway, wildcard DNS/TLS, private-preview auth | `crates/voie-pack/**`, `crates/voie-app-init/**`, `crates/voie-fabricd/**`, `ansible/**`, `infra/tofu/r0/dns.tf` | P1-C2 |
| 3 | Dedicated Database realization, Environment bindings, migration, backup/restore | `crates/voie-cloud/**`, `crates/voie-fabricd/**`, `nix/runtime/voie-postgres-image.nix` | P1-C3 |
| 4 | Exact Release promotion, approvals, health-gated cutover, restart, rollback | `crates/voie-cloud/**`, `crates/voie-fabricd/**`, `web/**` | P1-C4 |
| 5 | Quotas, cleanup, log bounds, metrics, egress proxy, suspension, retention, deletion proof | same product paths | P1-C5 |

### Profile 1 non-goals

```text
arbitrary Dockerfiles or user container images
Kubernetes YAML, Helm, or kubectl for users or agents
GitHub Actions
required Git repository or CI pipeline
multi-Fabric scheduler
multi-node K3s
automatic horizontal scaling
active-active application replicas
multi-region
custom domains
per-branch previews
serverless functions
generic cron and worker framework
message queue service
generic persistent application volumes
service mesh
user-defined ingress policies
shared multi-tenant PostgreSQL cluster
PostgreSQL HA or point-in-time recovery
automatic database rollback with code rollback
browser terminal or PTY
background process control in Workspace
cloud or Kubernetes credentials in user workloads
```
