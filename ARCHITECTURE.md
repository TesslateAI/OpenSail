# VOIE Cloud architecture

This file contains the accepted integrated design only: DSH browser UI and native management UI, canonical conversation/event/resource model, one product code path for local and Azure estates, and the Profile 1 Application platform that extends those boundaries.

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
| Fabric accepted typed specs, volume allocations, at-most-once journals | Fabric SQLite |
| current Pod/CRI/Firecracker/LVM observation | live substrate |
| Workspace bytes | dedicated Workspace thin LV (16/32/64 GiB virtual) while the Workspace is active |
| Database bytes | local linear LV while the Database is active |
| Firecracker/containerd runtime snapshots | 64 GiB Fabric `runtime` thin pool; not a product volume |
| Fabric Workspace pool | 264 GiB `workspace` thin pool; 128 GiB normal logical + 64 GiB restore headroom + 72 GiB safety/churn; no staging LV |
| Fabric linear budget | exact Database and Deployment LVs, 96 GiB on Fabric-1 |
| Fabric recovery reserve | 48 GiB physically unallocated VG extents |
| Release artifact | immutable Blob object |
| Application Deployment bytes | disposable 1 GiB local LV copied from the Release |
| Workspace and Database restore points | immutable Blob objects |
| Deployment logs | Blob, with a short local buffer only |
| DSH browser UI state | disposable projection |
| estate intent and OpenTofu state | private remote estate state |

```text
PostgreSQL says WHAT the product should be.
Fabric SQLite says WHAT this Fabric accepted and owns.
The live substrate says WHAT currently exists.
Reconciliation moves observed reality toward desired state.
Kubernetes is not the product database.
Blob is not the conversation control database.
PostgreSQL is not a duplicate transcript store.
Headscale is not product identity.
Audit is not an execution dependency.
```

Desired and observed are separate fields on every reconciled resource (`desired_revision` / `observed_revision`, `desired_state` / `observed_state`). Drift is `desired_revision > observed_revision`. Process-lifecycle adjectives are not extra product states.

## Reconciliation and at-most-once

Effects that are safe to repeat (Workspace/Database/Deployment present or absent, security profile, derived routes and NetworkPolicy) are reconcilers: persist typed desired spec, observe, plan, execute, observe again. There is no `unknown` no-replay problem because the operation is `make reality equal this spec`.

Effects that must not repeat keep the existing journal:

```text
accepted -> dispatched -> terminal
                       -> unknown
```

That journal is for Workspace exec, tenant migration, Release build, backup capture, model invocation, and canonical event append. It is not used for ordinary desired-state convergence. Restore and cutover use an isolated candidate; an ambiguous candidate is discarded.

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

At-most-once journals enforce unique `(workspace_id, call_id)` plus request hash:

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

## Profile 1 — Application platform

Profile 1 extends Profile 0. It does not replace it with a generic PaaS, arbitrary Kubernetes interface, OCI build system, CI platform, or service-broker framework.

### Product nouns

```text
Project         collaboration and authorization scope (existing `projects` table)
Application     one agent-managed software project
Workspace       mutable development filesystem and execution environment
Release         immutable application bytes and manifest
Environment     fixed dev or prod target
Deployment      realization of one Release in one Environment
Database        optional dedicated PostgreSQL instance for one Environment
```

The console may label an Application as “Project”. Internally it remains distinct from `projects`. An activation cannot select another Project or Workspace; `application.create` attaches to the inherited `project_id` and `workspace_id`.

Profile 1 limits, one Application:

```text
one Workspace
one HTTP application process
one dev Environment
one prod Environment
zero or one PostgreSQL Database per Environment
one active Deployment per Environment
one fixed Fabric
```

### Control and data plane

```text
Browser / VOIE Console
  -> Public Caddy
       console host              -> voie-cloud
       *.dev.<console-host>      -> private/public preview
       *.prod.<console-host>     -> production application
  voie-cloud
       identity, Project authorization
       Application / Environment / Release / Deployment authority
       Database and secret binding authority
       durable Run supervision
       private-preview authentication
       PostgreSQL control state
       Blob release objects, logs, backups, event bytes
       Key Vault secret values
  Headscale + exact product mTLS
  voie-fabricd
       Workspace, Release, Deployment, Database realization
       local operation journals
       desired/observed reconciliation
       local route realization
  K3s + Cilium
       trusted platform gateway Pod (exact Host -> active Environment Service)
       Workspace Firecracker VM  (mutable /workspace)
       Application Firecracker VM (fixed runtime, immutable /app, ephemeral /tmp)
       PostgreSQL Firecracker VM (dedicated durable volume)
```

Application request traffic does not pass through the normal `voie-cloud` HTTP handler. `voie-cloud` remains the authority and private-preview authentication source. Caddy and the Fabric gateway carry the data plane. The Fabric gateway is a derived data-plane component, not a second product authority (D001).

### Runtime profiles

Applications cannot supply an image name. Deployment-owned, Nix-built, versioned profiles:

```text
voie-workspace:v1   pinned development/build toolchain plus voie-runner and voie-pack
voie-app:v1         smaller fixed application runtime plus voie-app-init
voie-postgres:v1    fixed PostgreSQL database runtime
```

The C1 proof image (`voie-runner:c1`) remains the Profile 0 guest. It is not mutated into an uncontrolled image.

### Workspace, Release, Deployment

Workspace is where the agent changes code. Release is what the platform trusts as immutable input. Deployment is what the platform supervises.

A Release is produced from one exact Workspace generation: validate `voie.toml`, run typed test and build operations inside the guest, package with `voie-pack`, hash, write an immutable Blob object, commit metadata. Ready Release fields never change.

Production publication promotes an existing Release. It must not rebuild. Production bytes equal the previewed Release hash.

A candidate Deployment becomes `active` only after materialization, start, readiness, internal HTTP probe, stable Service selector switch, and a request through the real wildcard edge. Failed pre-switch checks leave the previous Deployment active.

### Manifest

One small `voie.toml` at the Application root declares runtime profile, build/test/run argv, one HTTP port, health path, optional PostgreSQL, and resource selection from platform limits. It must not name a container image, Kubernetes YAML, host path, privileged mode, service account, network namespace, volume device, Fabric identity, cloud resource identifier, or arbitrary ingress.

`voie-app-init` sets `/app`, executes one foreground child, forwards signals, reaps descendants, exits when the child exits, and never restarts or offers a shell. Kubernetes supervises the Pod.

### Packaging and Blob

`voie-pack` is a fixed guest helper. It opens only the validated Application root, rejects `..` and absolute paths, rejects escaping symlinks and special files, applies hard exclusions and optional `.voieignore`, enforces file-count and byte limits, and produces deterministic `tar.zst` while hashing.

Blob key: `releases/<project-id>/<application-id>/<sha256>.tar.zst`. `voie-cloud` writes and reads the object. Neither Workspace nor Fabric receives a Blob credential.

Release uniqueness is `(application_id, build_intent_id)` plus a request hash covering workspace id, generation, manifest hash, runtime profile, build/test commands, and output path. Same hash returns the existing result; a different hash conflicts; ambiguous dispatched work becomes `unknown` and is never replayed.

### Networking and preview authentication

Wildcard DNS and certificates, provisioned at estate deployment with DNS validation:

```text
*.dev.<console-host>
*.prod.<console-host>
```

The Fabric gateway is one trusted platform Caddy Pod. `voie-fabricd` generates its route map from slug, Environment kind, active Deployment, and Fabric-owned Service name. Unknown hosts return 404. No user Caddy fragments are accepted.

The console session cookie is not widened to Application subdomains. Private preview uses an exact-host `__Host-voie-preview` cookie issued through a one-time console code bound to `user_id`, `application_id`, `environment_id`, exact hostname, and short expiration. The edge strips the platform cookie and reserved names before forwarding, preserves Application cookies, and strips internal routing headers.

### Database, secrets, approvals

Each Environment may have one dedicated PostgreSQL Firecracker instance. Desired `security_profile` is reconciled until live PostgreSQL roles match; a guest marker is not authoritative. Credentials live in Key Vault or the encrypted backend; PostgreSQL stores only the secret reference. Production credentials never enter Workspace, build, dev Deployment, model prompt, tool result, canonical events, audit payload, or deployment log metadata.

Reuse Project-scoped `user_secrets`. Bind names onto Environments. A member may operate private development Deployments. Only owner/admin bind or rotate production secrets. Platform administration does not imply Project secret access.

Typed durable approvals: `publish_production`, `make_environment_public`, `bind_production_secret`, `restore_database`, `delete_database`, `delete_application`, `increase_resource_tier`. An unambiguous user statement is a valid approval when Application, Environment, and Release are unambiguous.

Release 0 approval is an explicit durable human authorization event, not mandatory separation of duties. An Admin or Owner with `ManageProduction` may accept an approval they requested. `requested_by` and `accepted_by` exist for attribution. The product does not implement four-eyes or independent second-person approval.

### Agent tools

Server-side tools in addition to bounded Bash. Every call derives `project_id`, `workspace_id`, `actor_user_id`, and `run_id` from activation context. The model may name an Application in the bound Project. It cannot supply another Project ID, Fabric ID, Workspace/database/Blob/Key Vault endpoint, or model endpoint.

Long builds, tests, migrations, and packaging use typed operations with server-selected deadlines. Bash stays bounded and foreground-only.

### Authorization

Existing Project roles:

```text
viewer  read Application state, Deployments, health, permitted logs
member  edit through agent, build Releases, deploy private dev previews
admin   visibility, Environment bindings, Databases, production Deployments
owner   all admin rights, destructive deletion, ownership-sensitive changes
```

### Profile 1 state ownership

| State | Sole authority |
|---|---|
| Applications and slugs | PostgreSQL |
| Environments and visibility | PostgreSQL |
| Release metadata and hashes | PostgreSQL |
| Release artifact bytes | immutable Blob objects |
| Deployment desired state | PostgreSQL |
| Deployment realization journal | Fabric SQLite |
| Active route intent | PostgreSQL |
| Realized Fabric route map | Fabric SQLite and generated gateway config |
| Application logs | immutable Blob chunks |
| Log ordering and metadata | PostgreSQL |
| Database metadata | PostgreSQL |
| Database realization | Fabric SQLite and live substrate |
| Database bytes | local block volume |
| Database backups | immutable Blob objects |
| Secret metadata and bindings | PostgreSQL |
| Secret values | Key Vault or encrypted local backend |
| Workspace source bytes | local Workspace block volume |
| Current Pods and Services | observed substrate only |

Kubernetes remains not the product database. Blob remains not the control database.

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
