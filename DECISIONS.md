# Load-bearing decisions

This file records only decisions that change a trust boundary, state owner, persistent model, external contract, release scope, or deployment owner.

## D001 — One Rust product process

Release 0 uses one Rust `voie-cloud` process for the public API, product authority, Session supervision, model proxy, static console assets, and Fabric client. Separate services require a measured isolation or scaling need.

## D002 — No product capability graph

Human authorization is fixed project membership and typed role checks. Agents are project-owned records. Normal execution does not depend on renewable tokens, grant intersection, or capability refresh.

## D003 — Activations receive an inherited local connection

A disposable activation is bound by server-side connection context. It receives no replayable bearer and cannot name another Project, Session, Run, or Workspace through its RPC surface.

## D004 — One selected Fabric, no scheduler

Profile 0 binds each Workspace to one Fabric selected by deployment configuration. The schema may hold more Fabrics later, but Release 0 contains no automatic placement or migration.

## D005 — Portable external Firecracker is the only runtime

The runtime contract is x86_64 KVM -> NixOS -> K3s -> Cilium -> patched Kata runtime-rs -> jailed Firecracker. Cloud-provider origin is not part of the contract.

## D006 — Trusted durable state is split by responsibility

PostgreSQL owns control metadata and ordering. Blob owns canonical immutable Session event bytes and other immutable recoverable objects. The Fabric owns local realization state. Local block volumes own active Workspace and Database bytes. D024 records the exact Fabric allocation and Blob recovery shape.

## D007 — Deployment ownership is exclusive

OpenTofu provisions resources, NixOS installs exact software and units, and Ansible supplies estate-specific configuration and live convergence. No layer writes another layer's state.

## D008 — No persistent local estate directory

Git contains reusable source only. Deployment-specific non-secret data lives in private remote estate state; secret values live in a secret backend; tool-specific files are generated ephemerally.

## D009 — Audit cannot stop valid work

Audit is metadata-only and append-oriented. Audit write or export failure must not invalidate identity, expire an Agent, or terminate an already accepted Run.

## D010 — Build the native VOIE Console

The browser surface is a native VOIE React application owned by this repository (`web/`, package `@voie/web`) and served same-origin by `voie-cloud` on its authenticated HTTPS origin. There is no separate web process, no second browser origin, and no third-party or legacy browser shell lineage. The console is a disposable projection: credentials, canonical Session state, and recovery authority never move into the browser.

## D011 — Preserve the browser contract shape

The console reads an authoritative baseline plus bounded long-poll events, discards stale cursors, refetches the baseline on gap, and performs single-attempt mutations that the browser never replays. Operation and event shapes are adapted only where the product model requires it: the server resolves Project, Agent, model, Workspace, and Fabric, and the browser never supplies endpoints, Fabric identity, or protected credentials.

## D012 — Console and activation dependency islands stay separate

The console (`web/`) and the activation (`activation/`) are separate pnpm workspaces and receive separate lockfiles when dependencies are imported, so peer resolution cannot silently couple their graphs.

## D013 — One canonical event/run model

A Run is durable PostgreSQL state advanced only by `voie-cloud`; an activation observes within its inherited connection but never owns Run state. Session history is one canonical event stream of deterministic immutable Blob objects ordered by PostgreSQL references. Console replay, activation results, recovery, and audit read the same canonical bytes; no second transcript format exists.

## D014 — Local and Azure estates share one code path

The identical `voie-cloud`, activation, console, and `voie-fabricd` binaries serve a local KVM estate and an Azure-hosted estate. Estate origin differs only in OpenTofu/NixOS/Ansible inputs. Product code never branches on environment and carries no optional or fail-open assembly paths per estate.

## D015 — Project remains the authorization scope; Application is the deployable

Profile 1 keeps `projects` as the collaboration and authorization table and adds `Application` as the agent-created deployable software project. Activation authority stays the inherited connection context. An activation cannot gain account-wide authority or select another Project.

## D016 — Application data plane is Caddy and the Fabric gateway

User Application HTTP does not enter the `voie-cloud` product handler. `voie-cloud` remains the sole product authority and private-preview authentication source. The Fabric gateway is a derived route-realizing Pod, not a second control process.

## D017 — Fixed versioned runtime profiles; no user images

Workspace, application, and PostgreSQL guests run deployment-owned Nix-built profiles (`voie-workspace:vN`, `voie-app:vN`, `voie-postgres:vN`). Applications cannot supply an image name. A later profile is `vN+1`; recorded Releases stay deployable on their original profile.

## D018 — Workspace mutates; Release is immutable; Deployment is supervised

The agent changes bytes only in a Workspace. A Release is an immutable snapshot of one Workspace generation. Production and preview never serve from the mutable Workspace. A Deployment realizes one Release in one Environment.

## D019 — Production publishes the exact previewed Release

Production publication promotes an existing Release hash. It must not rebuild. Candidate cutover is health-gated and atomic; a failed gate leaves the previous Deployment active. Rollback creates a new Deployment of an earlier Release rather than mutating the old row back to active.

## D020 — Dedicated Database per Environment

Profile 1 uses one dedicated PostgreSQL Firecracker instance per Application Environment, not a shared multi-tenant cluster. Production credentials never enter Workspace, build, development Deployment, model context, or canonical conversation events.

## D021 — Preview sessions are exact-host, not a widened console cookie

The console authentication cookie is not valid on Application subdomains. Private preview uses a host-only `__Host-voie-preview` cookie bound to one Application hostname and Environment, issued through a short-lived console code.

## D022 — Blob remains the Release-byte credential owner

Release artifacts, logs, and database backups are immutable Blob objects written and read by `voie-cloud`. Fabric and Workspace receive authorized streams, never Blob account credentials.

## D023 — `voie-app-init` is process behavior only

The application guest runs one fixed init that executes a single foreground argv, forwards signals, reaps descendants, and exits with the child. It does not restart, provide a shell, or expose a remote command interface. Kubernetes supervises the Pod.

## D024 — Allocate active mutable bytes locally; put durable immutable bytes in Blob

One Fabric data device holds one volume group. The 64 GiB `runtime` thin pool is only for containerd/Firecracker snapshots. Workspaces use a dedicated `workspace` thin pool (264 GiB data): logical virtual sizes of 16/32/64 GiB, with a 128 GiB normal logical budget, 64 GiB restore-candidate headroom, and 72 GiB safety/churn headroom that is not a user quota. There is no Fabric staging LV. Immutable Release, backup, restore, and snapshot bytes stream through `voie-cloud` and Blob. Databases and Deployments remain ordinary linear LVs with a 96 GiB normal budget — the physical remainder on a 475 GiB Fabric-1 VG after 64+1 runtime, 264+2 workspace, and 48 GiB recovery reserve. 48 GiB stays physically unallocated as a recovery reserve (largest Database restore plus a 16 GiB emergency floor). There is no uncontrolled overcommit and no continuous filesystem sync.

Platform storage tiers are selected by VOIE, not `voie.toml`:

```text
Workspace          16 GiB default, 32 GiB large, 64 GiB elevated
Development DB      8 GiB default, 16 GiB elevated
Production DB      16 GiB default, 32 GiB elevated
Deployment          1 GiB fixed
```

A newly created Workspace is a 16 GiB virtual thin LV. It grows 16→32 GiB automatically under guest disk pressure when the 128 GiB logical budget allows it. 32→64 GiB requires `increase_resource_tier` approval. Workspaces never shrink.

A Release is ready after the immutable Blob object and PostgreSQL metadata commit. Deployment materializes a private 1 GiB LV from Blob. There is no permanent local Release LV. Kubernetes PV/PVC capacity mirrors an already allocated LV; Kubernetes does not allocate.

Workspace and Database restore always allocate a candidate LV and switch only after proof. Workspace restore candidates come from the Workspace thin pool and are charged to the 64 GiB restore headroom until promotion. Database restore candidates are linear LVs. Suspend keeps local volumes. Archive fences Workspace execution, stops traffic, persists Blob restore points, then releases local capacity. Delete does not create a final backup.

Blob is the off-Fabric recovery copy: ZRS, soft delete, no versioning, product-owned retention. Azure Archive tier is not used. Control PostgreSQL keeps a 32 GiB allocation with 14-day managed backup retention. The unused control VM data disk is not part of the product.

## D025 — Desired spec, observation, and reconciliation

PostgreSQL says what the product should be. Fabric SQLite says what this Fabric has accepted and which local volumes it owns. The live substrate says what currently exists. Reconciliation is the deterministic function `plan(desired, local, observed)` that moves reality toward the desired spec.

Repeatable effects (resource present/absent, security profile, routes, NetworkPolicy) are reconciled with bounded backoff. They are not `unknown` no-replay journals. The accepted/dispatched/terminal/unknown journal remains only for at-most-once effects: Workspace exec, tenant Application migration, Release build/test/pack, backup capture, model invocation, and canonical event append.

Restore and Deployment cutover materialize an isolated candidate, prove it, then switch. Ambiguous candidate realization discards the candidate and leaves the active object unchanged.

Control never stores rendered Kubernetes YAML as recovery truth. Fabric persists a typed desired spec before local effects and recreates disposable Pods, PVs, and policies from that spec. Database security is a desired `security_profile` on the Database resource, not a special `database/secure` operation. Actual PostgreSQL observation is authoritative for SecurityReady; a guest marker may only optimize startup.

Kubernetes decides nothing about product truth. Session/Run stay on the existing no-replay event path and are not resource reconcilers.
