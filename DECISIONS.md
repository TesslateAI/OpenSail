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

PostgreSQL owns control metadata and ordering. Blob owns canonical immutable Session event bytes. The Fabric owns local realization state. Local block volumes own Workspace bytes.

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
