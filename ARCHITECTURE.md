# VOIE Cloud Release 0 architecture

This file contains the accepted integrated design only: DSH browser UI and native management UI, canonical conversation/event/resource model, and one product code path for local and Azure estates.

## Product path

```text
DSH browser UI
  -> HTTPS session (native credentials or external identity provider)
  -> VOIE-owned carrier
  -> voie-cloud on one NixOS control VM
       -> PostgreSQL: product control state
       -> Blob: canonical conversation event bytes
       -> Key Vault: protected credentials
       -> disposable activation over inherited local socket
       -> Headscale transport + product mTLS
            -> voie-fabricd on one NixOS KVM host
                 -> K3s -> Cilium -> patched Kata runtime-rs
                 -> jailed Firecracker VM
                 -> local block Workspace
                 -> credentialless voie-runner
```

One repository produces several binaries because trusted control, local machine authority, and hostile execution are separate security boundaries.

## Component boundaries

| Component | Owns | Must not own |
|---|---|---|
| `voie-cloud` | identity (native credentials, external identity provider links), projects, conversations, internal Sessions/Runs, canonical event persistence, model proxy, Fabric client, DSH browser API/assets, management UI API | Kubeconfig, LVM, containerd, host root |
| DSH browser UI (`web/`) | disposable projection and explicit user intent | credentials, canonical conversation state, recovery authority |
| activation | one Run's tool loop over one inherited connection | reusable token, database/storage/Fabric/model credentials |
| `voie-fabricd` | local SQLite, block reservations, K3s/Cilium/Firecracker realization, exec journal | human roles, cloud databases, model/identity-provider credentials |
| `voie-runner` | bounded foreground shell inside the guest | PTY, background jobs, persistent shell, credentials |

## DSH browser UI and management UI

The DSH conversation UI in `web/` (package `@voie/web`) is the canonical agent interaction surface, adapted through a VOIE-owned carrier (connection-voie) with same-origin cookie auth; it is not a port of any third-party or legacy browser shell. `voie-cloud` builds and serves it as static assets on its authenticated HTTPS origin; there is no separate web process and no second browser origin.

The DSH browser UI keeps this operation shape:

```text
conversation.list
conversation.show
conversation.history
conversation.create
conversation.prompt
conversation.inspect
conversation.cancel
conversation.recover
conversation.acknowledge
events.poll
```

DSH browser UI contract:

```text
authoritative baseline read plus bounded long-poll events
stale-cursor discard and baseline refetch
single-attempt mutations never replayed by the DSH browser UI
deterministic unminified build; no sourcemaps
strict CSP and static-asset serving rules
```

The server resolves Project, Agent, model, Workspace, and Fabric. The DSH browser UI never supplies a model endpoint, Workspace endpoint, Fabric identity, or protected credential. Refresh/reconnect never resubmits intent.

The management UI is a native application, distinct from the DSH browser UI. Agent interaction happens only in the DSH browser UI.

The DSH browser UI and the activation are separate pnpm workspaces. Each receives its own lockfile when dependencies are imported.

## Identity and authorization

Authentication identities are:

```text
User
Fabric
```

Fabric is a platform-admin identity. The User is provider-independent: no identity provider owns the User record. Native credentials and external identity providers both authenticate a VOIE User; human OIDC login is optional and default-off. `User.id` is the durable internal identity; `auth_identities` maps provider credentials to users; authorization never inspects provider claims.

Human path:

```text
native credentials -> User
external identity provider -> linked User
opaque server-side cookie -> Web session
Team membership and Personal scope -> owner | admin | member | viewer
typed action -> role, Personal/Team scope, and resource scope check
```

Project storage currently implements Personal/Team collaboration scopes (`projects.kind` = `personal` | `team`); Fabrics are platform-admin infrastructure. A Personal project is a single user's scope; a Team project is a multi-user collaboration scope.

An Agent is a project-owned configuration row, not an IAM principal. DSH browser UI logout or cookie expiry does not cancel an accepted Run. Cancel, disable, membership removal, or project suspension are explicit durable transitions.

Activation authority is the server-side context of its inherited connection:

```text
project_id
agent_id
session_id
run_id
workspace_id
writer_generation
```

These identifiers are not selected by activation requests.

Fabric authentication is:

```text
Headscale = private reachability
product mTLS certificate = exact fabric_id
```

There is no principal tree, generic grant language, signed product capability, session scope, soft/hard TTL, container-to-principal map, or refresh loop.

## Canonical conversation/event/resource model

Product resources:

```text
User
Project
Agent
Conversation
Workspace
ExecCall
AuditEvent
```

Session and Run are internal implementation: a Session carries one Conversation's state; a Run advances one Session. Fabrics are platform-admin infrastructure, not product resources.

Profile 0 relationships:

```text
Agent belongs to one Project
Conversation binds one Agent and one Workspace
Session carries one Conversation's state
Run advances one Session
Workspace has one fixed Fabric
Workspace execution generations are disposable
Workspace bytes are durable
```

A Run is durable server-side state owned by PostgreSQL. A Run advances only through transitions recorded by `voie-cloud`; an activation observes and reports within one inherited connection but never owns Run state.

Conversation history is one canonical event stream. Every append produces a deterministic immutable object in Blob; PostgreSQL holds the ordered references and the head. The DSH browser UI replay path, the activation result path, recovery, and audit all read the same canonical bytes. There is no second transcript format.

There is no public Computer resource and no scheduler.

## State ownership

| State | Sole authority |
|---|---|
| users, Web sessions, projects, roles | PostgreSQL |
| Agents, Sessions, Runs, Fabrics, Workspaces | PostgreSQL |
| Session head, writer and attention generations | PostgreSQL |
| canonical conversation event bytes | immutable Blob objects |
| ordered Blob references | PostgreSQL |
| cloud exec journal and audit index | PostgreSQL |
| protected secrets and CA keys | Key Vault |
| Fabric realization, reservations, local exec journal | Fabric SQLite |
| current Pod/CRI/Firecracker observation | Fabric SQLite + live substrate |
| Workspace bytes | local block volume |
| DSH browser UI state | disposable projection |
| estate intent and OpenTofu state | private remote estate state |

```text
Kubernetes is not the product database.
Blob is not the conversation control database.
PostgreSQL is not a duplicate transcript store.
Headscale is not product identity.
Audit is not an execution dependency.
```

## Session durability and writer fencing

Append order:

```text
verify writer_generation and expected revision
 -> verify stable append_id + content hash
 -> write deterministic immutable Blob object
 -> commit reference and new head in PostgreSQL
 -> resolve activation append
 -> allow external effect
```

An identical append retry returns the committed revision. Reusing an append ID with different content is a conflict. An unreferenced Blob is an orphan, never canonical history.

One Session writer is concurrency control, not IAM. `voie-cloud` holds a PostgreSQL advisory lock and advances a monotonic writer generation. Process death releases the lock with the database connection.

## Exec no-replay

Cloud and Fabric journals enforce unique `(workspace_id, call_id)` plus request hash:

```text
accepted -> dispatched -> terminal
                       -> unknown
```

`dispatched` is durable before one execution attempt. A repeated call ID:

```text
different request hash -> conflict
terminal -> return retained result
dispatched/unknown -> outcome-unknown; never dispatch again
```

The claim is at-most-one dispatch attempt, not exactly-once execution.

## Network and credentials

Every Workspace starts with default-deny ingress and egress. Profile 0 admits only deployment-approved destination CIDR and TCP port. Desired and observed revisions remain separate.

The guest receives no Azure, Kubernetes, Headscale, Fabric, PostgreSQL, Blob, model, identity-provider, or Git credential. The guest image is one fixed profile built by the deployment; no arbitrary guest images exist.

## Deployment and estate ownership

```text
OpenTofu provisions provider resources.
NixOS installs exact software and defines units/host shape.
Ansible supplies estate configuration and converges live services.
voie-cloud owns product state.
voie-fabricd owns local runtime state.
```

OpenTofu runs no remote commands. Nix contains no estate facts or secret bytes. Ansible installs no mutable software outside Nix and expresses no provisioning state machine in shell.
Fabrics are platform-admin infrastructure, provisioned and operated by the platform.

Local and Azure estates run the identical product code path. The same `voie-cloud`, `voie-fabricd`, activation, and DSH browser UI and management UI builds serve both; a local KVM host and an Azure-hosted host differ only in OpenTofu/NixOS/Ansible inputs. No product code branches on estate origin, and no environment-specific behavior exists outside deployment configuration.

A zero-touch Fabric host must already run the approved NixOS baseline or expose automated rescue, PXE, Redfish, or equivalent boot authority.

```text
Git                 reusable source only
remote estate state deployment intent, provider state, host attachment, release
secret backend       secret values only
/run/user/...        disposable generated tool inputs
```

There is no persistent untracked deployment directory and no secret or encrypted secret in Git.
