# Deploying VOIE Cloud (Release 0)

One-shot deployment from this repository to the live Azure control VM and
the baremetal fabric host.

## Prerequisites

1. A local Linux machine with `nix` (the flake devshell carries tofu,
   ansible, node, and the rest of the toolchain).
2. `ssh baremetal-1-cs` works directly — the fabric deploy targets that
   literal alias; no underlying host discovery happens.
3. A deployment env file. Copy `template.env`, fill in real values,
   `chmod 600`, and keep it outside git.
4. If the deploy identity cannot list storage keys (WAF-blocked), also
   pass `VOIE_TF_BACKEND_HCL` pointing at a backend config fragment that
   carries the tfstate `access_key` (container/key names included). The
   recipe consumes the file by path; the key value never enters the env,
   logs, or repo.

## One-shot deploy

```bash
export VOIE_C7_ENV_FILE=/path/to/your.env
just live-c7
```

The recipe:

1. Builds/refreshes the Azure control VM image (NixOS, pinned in the flake).
2. Initializes OpenTofu with the backend config (or operator fragment).
3. **Plans to a file and inspects it** — prints destroy/replace counts and
   refuses a destructive apply unless `VOIE_C7_WIPE=1` is set.
4. Applies the saved plan.
5. Converges the control VM and fabric host with Ansible; native auth is
   the default (human OIDC is optional and disabled unless
   `VOIE_OIDC_PROVISION=true`).
6. Stages the bootstrap admin password as a 0600 file (from
   `VOIE_BOOTSTRAP_ADMIN_PASSWORD`; Key Vault only as a legacy fallback).
7. Runs `tests/live/native-c6.sh`: native login → Personal scope →
   Workspace → atomic Conversation → model call → remote Bash in the
   Firecracker Workspace → canonical events → follow-up → reconstruction.

## Post-deploy proof

```bash
export VOIE_CONTROL_URL=https://<public_hostname>
export VOIE_BOOTSTRAP_ADMIN_USERNAME=admin
export VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE=/secure/0600/password-file
just live-c7-proof
```

## Browser acceptance

```bash
export VOIE_SMOKE_ORIGIN=https://<public_hostname>
export VOIE_SMOKE_USER=admin
export VOIE_SMOKE_PASSWORD_FILE=/path/to/0600/password-file
just browser-smoke
```

The harness drives 16 real browser steps (login → portal → scope →
workspace → New chat → first message → live tool events → queued
follow-up → reload reconstruction) against the deployed built assets.

## C8 live proof

`just live-c8-preclose` proves isolation, unknown/no-replay, recovery, restore,
and cleanup while public management SSH is still open. That is a diagnostic.

`just live-c8` is the checkpoint: it runs the same proof, then closes public
management TCP/22 according to the accepted deployment design and verifies
closure from outside. Operator management over the private route must remain
usable.

## Security rules

- Never print, commit, or log any value from the env file.
- The deployment parser is data-only: it allowlists variable NAMES and
  never evaluates shell fragments from the file.
- The Firecracker guest receives none of the Azure/PostgreSQL/Blob/Key
  Vault/model credentials; they stay in the trusted control.
