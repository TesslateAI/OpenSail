check: repo-hygiene estate-check
    cargo fmt --check
    cargo check --workspace --locked
    pnpm --dir activation run typecheck
    pnpm --dir web install --frozen-lockfile
    pnpm --dir web run typecheck
    pnpm --dir web build
    git diff --check

# Explicit developer provisioning of the immutable activation entry. The
# activation runtime itself never installs or builds anything.
activation-dist:
    pnpm --dir activation install --frozen-lockfile
    pnpm --dir activation run build

# Real PostgreSQL + Azure Blob + model + mTLS Fabric. The recipe fails closed
# when any live boundary is not configured.
live-c3:
    bash tests/live/cloud-c3.sh

# Real Node activation -> model -> remote Bash -> durable result.
live-c4:
    bash tests/live/activation-c4.sh

# Fresh Node activation resumes the same durable Session and Workspace.
live-c5:
    bash tests/live/activation-c5.sh

# Native console performs the real bootstrap-admin login -> prompt -> tool ->
# answer path against the served artifact; fails closed before any browser
# work.
live-c6:
    bash tests/live/native-c6.sh

# Read-only live preflight. Does not mutate. READY or a complete missing list.
live-doctor host="baremetal-1-cs":
    bash tests/live/live-doctor.sh {{host}}

# Profile 1 contract tests: Application schema, slug, no-replay Release,
# exact promotion, preview host binding, typed Fabric realization, and
# health-gated cutover. Live P1 checkpoints remain BLOCKED until their
# estate recipes exist.
test-p1-contract:
    cargo test -p voie-pack --locked
    cargo test -p voie-app-init --locked
    cargo test -p voie-cloud --test application_platform_contract --locked
    cargo test -p voie-cloud --test security_53_auth --locked
    cargo test -p voie-cloud --lib file_backend --locked
    cargo test -p voie-egress --locked
    cargo test -p voie-fabricd --test product_api --locked
    cargo test -p voie-fabricd --lib --locked
    python3 -m py_compile tests/live/p1-tracker.py
    python3 -m py_compile ansible/files/voie-disarm-legacy-rescue.py
    bash tests/caddyfile_preview_edge.sh
    bash tests/postgres_init_cluster_listen.sh
    bash tests/postgres_legacy_migrate.sh
    bash tests/disarm_legacy_rescue.sh
    bash tests/guest_image_bins.sh

# Load Profile 1 guest images into the live Fabric containerd. The host
# NixOS image-load unit on a Profile 0 estate only imports voie-runner:c1.
# Same import path as live-c1; does not restart fabricd or change mTLS pin.
live-p1-images host="baremetal-1-cs":
    ssh {{host}} 'test -w /dev/kvm'
    ssh {{host}} 'systemctl is-active --quiet k3s'
    nix build -L .#voie-gateway-image -o /tmp/voie-gateway-image.tar.gz
    nix build -L .#voie-postgres-image -o /tmp/voie-postgres-image.tar.gz
    nix build -L .#voie-app-image -o /tmp/voie-app-image.tar.gz
    nix build -L .#voie-workspace-image -o /tmp/voie-workspace-image.tar.gz
    cat /tmp/voie-gateway-image.tar.gz | ssh {{host}} 'k3s ctr -n k8s.io images import -'
    cat /tmp/voie-postgres-image.tar.gz | ssh {{host}} 'k3s ctr -n k8s.io images import -'
    cat /tmp/voie-app-image.tar.gz | ssh {{host}} 'k3s ctr -n k8s.io images import -'
    cat /tmp/voie-workspace-image.tar.gz | ssh {{host}} 'k3s ctr -n k8s.io images import -'
    ssh {{host}} 'k3s ctr -n k8s.io images ls' | grep -q 'voie-workspace:v1'
    ssh {{host}} 'k3s ctr -n k8s.io images ls' | grep -q 'voie-app:v1'
    ssh {{host}} 'k3s ctr -n k8s.io images ls' | grep -q 'voie-postgres:v1'
    ssh {{host}} 'k3s ctr -n k8s.io images ls' | grep -q 'voie-gateway:v1'

# Profile 1 live checkpoints. Each recipe fails closed (exit 2) when the
# KVM/K3s/Firecracker estate or control plane is absent. PASS belongs to
# the working branch/PR revision that ran the real recipe.
live-p1-c1 host="baremetal-1-cs":
    bash tests/live/p1-c1.sh {{host}}

live-p1-c2 host="baremetal-1-cs":
    bash tests/live/p1-c2.sh {{host}}

live-p1-c3 host="baremetal-1-cs":
    bash tests/live/p1-c3.sh {{host}}

# Residual #53 security proofs on the working-branch estate: gateway source
# restriction, legacy rescue gone, tenant postgres role, egress specials.
live-security-53 host="baremetal-1-cs":
    bash tests/live/security-53.sh {{host}}

live-p1-c4 host="baremetal-1-cs":
    bash tests/live/p1-c4.sh {{host}}

live-p1-c5 host="baremetal-1-cs":
    bash tests/live/p1-c5.sh {{host}}

# One disposable Workspace/Database/Deployment loss + explicit restore proof.
# Never touches keep-list identities.
live-desired-state-loss host="baremetal-1-cs":
    bash tests/live/desired-state-loss.sh {{host}}

# Opt-in external identity-provider variant of the live C6 path. Requires an
# OIDC-enabled control (VOIE_AUTH_MODE=oidc or both) and provider
# credentials; the native bootstrap admin stays the default acceptance path.
live-c6-oauth:
    bash tests/live/oauth-c6.sh

# Checkpoint C1: prove a real, qualified KVM/K3s/Kata jailed Firecracker guest
# executes `printf ok` through voie-runner and prints exactly `ok`.
#
# Every remote interaction goes through `ssh baremetal-1-cs` (override with
# `just live-c1 <host>`). The recipe is a linear sequence of typed assertions,
# each of which must pass before the next runs:
#
#   1. the host exposes writable KVM (hardware virtualization qualification);
#   2. k3s is active (Kubernetes qualification);
#   3. containerd registers handler kata-fc-rs-voie from the patched shim
#      (the rendered drop-in produced by nix/runtime/kata-runtime-rs.nix);
#   4. the voie-firecracker RuntimeClass selects that exact handler;
#   5. the runner image and pod manifest built from this tree are loaded;
#   6. the pod reaches Succeeded with exit code 0;
#   7. its entire log is exactly `ok`;
#   8. the Firecracker VMM it ran under was jailed: its host uid lies in the
#      per-sandbox non-root range allocated by the jailer-identity repair.
#
# Estate unavailable (host down, in rescue, or unprovisioned) is reported as
# such; nothing here degrades the assertions to fake a pass.
[doc("Prove C1: jailed Firecracker guest runs voie-runner -- printf ok")]
live-c1 host="baremetal-1-cs":
    ssh {{host}} 'test -w /dev/kvm'
    ssh {{host}} 'systemctl is-active --quiet k3s'
    ssh {{host}} 'grep -q "kata-fc-rs-voie" /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.d/voie-kata-fc-rs.toml'
    ssh {{host}} 'test "$(k3s kubectl get runtimeclass voie-firecracker -o jsonpath={.handler})" = "kata-fc-rs-voie"'
    nix build -L .#voie-runner-image -o /tmp/voie-runner-image.tar.gz
    nix build -L .#voie-c1-pod-manifest -o /tmp/voie-c1-pod.yaml
    cat /tmp/voie-runner-image.tar.gz | ssh {{host}} 'k3s ctr images import -'
    ssh {{host}} 'k3s kubectl delete pod voie-c1 --ignore-not-found --wait=false'
    cat /tmp/voie-c1-pod.yaml | ssh {{host}} 'k3s kubectl apply -f -'
    # While the guest holds the sandbox open (the trailing `sleep 20`), prove
    # the VMM is jailed under a non-root per-sandbox identity.
    ssh {{host}} 'set -e; for i in $(seq 1 45); do p="$(pgrep -x firecracker | head -1)"; [ -n "$p" ] && break; sleep 1; done; test -n "$p"; uid="$(stat -c %u /proc/$p)"; test "$uid" -ge 100000; echo "jailed firecracker pid=$p uid=$uid"'
    ssh {{host}} 'k3s kubectl wait pod/voie-c1 --for=jsonpath="{.status.phase}"=Succeeded --timeout=180s'
    ssh {{host}} 'test "$(k3s kubectl get pod voie-c1 -o jsonpath="{.status.containerStatuses[0].state.terminated.exitCode}")" = 0'
    ssh {{host}} 'test "$(k3s kubectl logs pod/voie-c1)" = ok'
    ssh {{host}} 'k3s kubectl delete pod voie-c1 --wait=false'

# Checkpoint C2: one block-backed Workspace survives Firecracker execution
# replacement. Every remote interaction goes through `ssh baremetal-1-cs`
# (override with `just live-c2 <host>`). The recipe is implemented by
# tests/live/fabric/live-c2.sh and must prove:
#
#   1. the same C1 Firecracker runtime identity (KVM, k3s, kata-fc-rs-voie,
#      RuntimeClass voie-firecracker);
#   2. E1 writes `printf marker > /workspace/marker` through voie-runner;
#   3. E1 is replaced by E2 (different Pod UID, same PV/device);
#   4. E2 `cat /workspace/marker` returns exactly `marker`;
#   5. the command never ran on the host (device not host-mounted; no
#      /workspace/marker on the host);
#   6. DELETE positively removes the owned Pod, reservation, jail, VMM, and
#      child processes.
#
# Estate unavailable (host down, in rescue, or unprovisioned) is reported as
# such; nothing here degrades the assertions to fake a pass.
[doc("Prove C2: workspace marker survives Firecracker E1 -> E2")]
live-c2 host="baremetal-1-cs":
    bash tests/live/fabric/live-c2.sh {{host}}

# Build the local KVM VM runner into XDG_RUNTIME_DIR. The result link and all
# generated VM state stay outside the checkout.
dev-fabric-build:
    #!/usr/bin/env bash
    set -euo pipefail
    runtime_base="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    case "$runtime_base" in
      /*) ;;
      *) printf 'just dev-fabric-build: XDG_RUNTIME_DIR must be absolute\n' >&2; exit 2 ;;
    esac
    runtime_root="$runtime_base/voie-fabric-dev"
    current_cgroup="$(sed -n 's/^0:://p' /proc/self/cgroup)"
    case "$current_cgroup" in
      */voie-dev-stack.slice/* | */voie-dev-stack.scope) ;;
      *) printf 'just dev-fabric-build: refusing an unscoped VM build\n' >&2; exit 2 ;;
    esac
    install -d -m 700 "$runtime_root"
    # Reuse a realized, patched Kata output when the workstation already has
    # one. The paths are discovered at runtime and passed as impure inputs;
    # no machine-specific store path is committed to the repository.
    kata_assets=""
    for candidate in /nix/store/*-kata-runtime-rs-assets-4.0.0; do
      config="$candidate/opt/kata/share/defaults/kata-containers/runtime-rs/configuration-rs-fc.toml"
      if test -f "$config" && grep -q '^jailer_uid_min = 100000$' "$config"; then
        kata_assets="$candidate"
        break
      fi
    done
    kata_shim=""
    for candidate in /nix/store/*-kata-runtime-rs-shim-4.0.0-*; do
      if test -x "$candidate/bin/containerd-shim-kata-v2"; then
        kata_shim="$candidate"
        break
      fi
    done
    # Keep local development bounded: the VM is 4 GiB and the image build
    # itself must not fan out enough derivations to exhaust the workstation.
    nix_args=(--extra-experimental-features 'nix-command flakes' build --max-jobs 1 --cores 2)
    if test -n "$kata_assets" && test -n "$kata_shim"; then
      export VOIE_KATA_ASSETS="$kata_assets"
      export VOIE_KATA_SHIM="$kata_shim"
      nix_args+=(--impure)
      printf 'using local Kata cache: assets=%s shim=%s\n' "$kata_assets" "$kata_shim"
    fi
    nix "${nix_args[@]}" \
      "{{ justfile_directory() }}#nixosConfigurations.fabric-dev.config.system.build.vm" \
      -o "$runtime_root/vm"
    printf 'local VM runner: %s\n' "$runtime_root/vm"

# Start exactly one local KVM VM. The root image, second block drive, serial
# output, and PID file are disposable runtime state under XDG_RUNTIME_DIR.
dev-fabric-up:
    #!/usr/bin/env bash
    set -euo pipefail
    # shellcheck disable=SC1091
    source "{{ justfile_directory() }}/dev-stack/pid-guard.sh"
    scope_prefix="${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}"
    runtime_base="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    case "$runtime_base" in
      /*) ;;
      *) printf 'just dev-fabric-up: XDG_RUNTIME_DIR must be absolute\n' >&2; exit 2 ;;
    esac
    runtime_root="$runtime_base/voie-fabric-dev"
    pid_file="$runtime_root/qemu.pid"
    log_file="$runtime_root/qemu.log"
    root="{{ justfile_directory() }}"
    install -d -m 700 "$runtime_root"
    tls_dir="$runtime_base/voie-dev-stack/tls"
    ca_bundle="$tls_dir/ca-bundle.pem"
    client_cert="$tls_dir/client-cert.pem"
    client_key="$tls_dir/client-key.pem"
    wait_fabricd_health() {
      local probe_pid="$1" attempts="${2:-900}" _
      for _ in $(seq 1 "$attempts"); do
        if ! kill -0 "$probe_pid" 2>/dev/null; then
          return 1
        fi
        if curl --fail --silent --connect-timeout 2 --max-time 5 \
          --cacert "$ca_bundle" --cert "$client_cert" --key "$client_key" \
          https://127.0.0.1:17840/v1/health >/dev/null 2>&1; then
          return 0
        fi
        sleep 1
      done
      return 1
    }
    live_qemu_pid() {
      local proc pid cmd
      for proc in /proc/[0-9]*; do
        pid="${proc#/proc/}"
        [[ "$pid" =~ ^[0-9]+$ ]] || continue
        cmd="$(cat "$proc/cmdline" 2>/dev/null | tr '\0' ' ' || true)"
        [[ -n "$cmd" ]] || continue
        case "$cmd" in
          *qemu-system-x86_64*voie-fabric-dev*)
            printf '%s\n' "$pid"
            return 0
            ;;
        esac
      done
      return 1
    }
    # The existing-VM guard runs BEFORE any TLS work: regenerating the PKI
    # under an already-running VM would rotate the server identity out from
    # under the live guest and break every later probe. A recorded PID is
    # not enough: fabricd must still answer, and a live QEMU whose pid-guard
    # record is stale must not have its disks deleted underneath it.
    if test -s "$pid_file" && pid_guard_validate "$pid_file" "$scope_prefix"; then
      vm_pid="$(cat "$pid_file")"
      printf 'local VM already running (pid %s)\n' "$vm_pid"
      if wait_fabricd_health "$vm_pid" 15; then
        printf 'local Fabric VM is ready (pid %s, mTLS API https://127.0.0.1:17840)\n' "$vm_pid"
        exit 0
      fi
      printf 'just dev-fabric-up: QEMU is running but fabricd health failed\n' >&2
      exit 1
    fi
    live_pid="$(live_qemu_pid || true)"
    if [[ -n "$live_pid" ]]; then
      printf 'just dev-fabric-up: live QEMU pid %s is not recorded; re-recording instead of resetting disks\n' "$live_pid" >&2
      pid_guard_record "$live_pid" "$pid_file" "$scope_prefix" || true
      if wait_fabricd_health "$live_pid" 15; then
        printf 'local Fabric VM is ready (pid %s, mTLS API https://127.0.0.1:17840)\n' "$live_pid"
        exit 0
      fi
      printf 'just dev-fabric-up: live QEMU is running but fabricd health failed; not deleting disks\n' >&2
      exit 1
    fi
    current_cgroup="$(pid_guard_cgroup "$$")"
    if ! pid_guard_scope_kind "$current_cgroup" "$scope_prefix" >/dev/null; then
      printf 'just dev-fabric-up: refusing to launch outside %s.slice; run the scoped fabric smoke recipe\n' "$scope_prefix" >&2
      exit 2
    fi
    pid_guard_discard "$pid_file"
    # The dev pool disk and devmapper metadata are recreated per launch. A
    # surviving root image after an unclean QEMU exit therefore has mixed
    # containerd snapshot names over a wiped pool, and CRI recreates an
    # empty pause chain faster than it can be seeded. Reset the root image
    # too so both disks always begin one generation together.
    if test -e "$runtime_root/root.qcow2"; then
      printf 'just dev-fabric-up: resetting root image and pool disk together\n' >&2
      rm -f "$runtime_root/root.qcow2"
    fi
    if ! test -x "$runtime_root/vm/bin/run-voie-fabric-dev-vm"; then
      if test "${VOIE_DEV_FABRIC_ALLOW_BUILD:-0}" != 1; then
        printf 'just dev-fabric-up: VM artifact is absent; refusing an implicit high-memory build\n' >&2
        printf 'run an explicitly bounded dev-fabric-build operation first\n' >&2
        exit 2
      fi
      just --justfile "{{ justfile_directory() }}/justfile" dev-fabric-build
    fi
    test -x "$runtime_root/vm/bin/run-voie-fabric-dev-vm"
    # qcow2 writeback cache fills the 8G slice with file pages; uncached
    # I/O keeps MemoryMax headroom for QEMU RSS and the rest of the stack.
    run_vm="$runtime_root/run-vm"
    sed -e 's/cache=writeback/cache=none/g' -e 's/"8192M"/"16384M"/g' \
      "$runtime_root/vm/bin/run-voie-fabric-dev-vm" >"$run_vm"
    chmod +x "$run_vm"
    # Product-shaped Fabric PKI from the shared runtime CA (dev-stack/tls.sh):
    # the guest receives its server identity, the dev CA, and the PUBLIC
    # client certificate (fingerprint pinning) through a read-only virtfs;
    # the private key never enters the VM. Generated only on real launches.
    tls_dir="$runtime_base/voie-dev-stack/tls"
    # shellcheck disable=SC1091
    bash "$root/dev-stack/tls.sh" gen >/dev/null
    ca_bundle="$tls_dir/ca-bundle.pem"
    client_cert="$tls_dir/client-cert.pem"
    client_key="$tls_dir/client-key.pem"
    guest_tls="$runtime_root/guest-tls"
    rm -rf "$guest_tls"
    install -d -m 700 "$guest_tls"
    install -m 644 "$tls_dir/fabric-server-cert.pem" "$guest_tls/fabric-server.crt"
    install -m 600 "$tls_dir/fabric-server-key.pem" "$guest_tls/fabric-server.key"
    install -m 644 "$tls_dir/ca.pem" "$guest_tls/fabric-ca.crt"
    install -m 644 "$tls_dir/client-cert.pem" "$guest_tls/fabric-client.crt"
    rm -rf "$runtime_root/vm-tmp"
    install -d -m 700 "$runtime_root/vm-tmp"
    # -nographic plus a closed/non-tty stdin (systemd-run --pipe) can leave
    # QEMU parked on the stdio monitor with no vCPU threads. Detach the
    # monitor and write the serial console to a file instead.
    NIX_DISK_IMAGE="$runtime_root/root.qcow2" \
      TMPDIR="$runtime_root/vm-tmp" \
      USE_TMPDIR=1 \
      QEMU_OPTS="-display none -monitor none -serial file:${log_file}.serial -virtfs local,path=$guest_tls,mount_tag=voie-pki,security_model=none,id=voie-pki" \
      "$run_vm" >"$log_file" 2>&1 </dev/null &
    vm_pid=$!
    # The NixOS VM runner exec(2)s into qemu-system within milliseconds;
    # recording before that transition would pin the pre-exec bash cmdline
    # and make every later validation compare unlike bytes, failing forever
    # on a perfectly healthy VM. Two stable bash reads could still race the
    # exec, so wait for the qemu-system argv itself, bounded.
    saw_qemu=""
    for _ in $(seq 1 50); do
      cur_cmdline="$(tr '\0' ' ' <"/proc/$vm_pid/cmdline" 2>/dev/null || true)"
      case "$cur_cmdline" in
        *qemu-system*) saw_qemu=1; break ;;
      esac
      if [[ -z "$cur_cmdline" ]]; then
        break
      fi
      sleep 0.1
    done
    if [[ -z "$saw_qemu" ]]; then
      printf 'just dev-fabric-up: VM runner did not exec into qemu-system\n' >&2
      kill "$vm_pid" 2>/dev/null || true
      wait "$vm_pid" || true
      cat "$log_file" >&2 || true
      exit 1
    fi
    pid_guard_record "$vm_pid" "$pid_file" "$scope_prefix" || {
      printf 'just dev-fabric-up: could not record VM ownership; refusing to continue\n' >&2
      exit 1
    }
    for _ in $(seq 1 900); do
      if ! pid_guard_validate "$pid_file" "$scope_prefix"; then
        if ! kill -0 "$vm_pid" 2>/dev/null; then
          wait "$vm_pid" || true
          printf 'just dev-fabric-up: QEMU exited before fabricd became ready\n' >&2
          cat "$log_file" >&2 || true
          exit 1
        fi
        # Identity sidecars can race a still-running QEMU; keep probing
        # health instead of waiting forever for a live VM to exit.
      fi
      if curl --fail --silent --connect-timeout 2 --max-time 5 --cacert "$ca_bundle" --cert "$client_cert" --key "$client_key" https://127.0.0.1:17840/v1/health >/dev/null 2>&1; then
        printf 'local Fabric VM is ready (pid %s, mTLS API https://127.0.0.1:17840)\n' "$vm_pid"
        exit 0
      fi
      sleep 1
    done
    printf 'just dev-fabric-up: fabricd health did not become ready\n' >&2
    cat "$log_file" >&2 || true
    exit 1

# Stop the local VM and remove only its disposable XDG_RUNTIME_DIR state.
dev-fabric-down:
    #!/usr/bin/env bash
    set -euo pipefail
    # shellcheck disable=SC1091
    source "{{ justfile_directory() }}/dev-stack/pid-guard.sh"
    scope_prefix="${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}"
    runtime_base="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    case "$runtime_base" in
      /*) ;;
      *) printf 'just dev-fabric-down: XDG_RUNTIME_DIR must be absolute\n' >&2; exit 2 ;;
    esac
    runtime_root="$runtime_base/voie-fabric-dev"
    pid_file="$runtime_root/qemu.pid"
    pid_guard_stop "$pid_file" "$scope_prefix"
    rm -rf "$runtime_root"
    printf 'local Fabric VM stopped\n'

# Run the local C1/C2-shaped API smoke and always tear down its VM. This never
# invokes the remote live recipes; failures leave the exact local QEMU log
# available until the teardown trap completes. Every API stage is labeled
# before its assertions, captures the HTTP status and body, and fails with
# that evidence attached; an `unknown` exec verdict is reported as such and
# never retried into a pass.
dev-fabric-smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    runtime_base="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    case "$runtime_base" in
      /*) ;;
      *) printf 'just dev-fabric-smoke: XDG_RUNTIME_DIR must be absolute\n' >&2; exit 2 ;;
    esac
    runtime_root="$runtime_base/voie-fabric-dev"
    cleanup() {
      status=$?
      if test "$status" -ne 0; then
        printf 'just dev-fabric-smoke: local QEMU log follows\n' >&2
        cat "$runtime_root/qemu.log" 2>/dev/null >&2 || true
      fi
      just --justfile "{{ justfile_directory() }}/justfile" dev-fabric-down >/dev/null 2>&1 || true
      exit "$status"
    }
    trap cleanup EXIT
    current_cgroup="$(sed -n 's/^0:://p' /proc/self/cgroup)"
    case "$current_cgroup" in
      */voie-dev-stack.slice/* | */voie-dev-stack.scope) ;;
      *) printf 'just dev-fabric-smoke: refusing to run outside the bounded stack slice\n' >&2; exit 2 ;;
    esac
    just --justfile "{{ justfile_directory() }}/justfile" dev-fabric-up
    tls_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/voie-dev-stack/tls"
    base="https://127.0.0.1:17840"

    stage() { printf '\n== fabric-smoke: %s ==\n' "$1"; }
    # Short-bound transport, reserved for readiness-style probes only.
    probe() {
      curl --fail --silent --show-error --connect-timeout 2 --max-time 5 \
        --cacert "$tls_dir/ca-bundle.pem" --cert "$tls_dir/client-cert.pem" \
        --key "$tls_dir/client-key.pem" "$@"
    }
    # api METHOD PATH MAX_TIME [JSON_BODY] sets HTTP_CODE and HTTP_BODY.
    # --fail is never used: the status line is evidence, not an exception.
    # Timeouts mirror server behavior: create/replace may wait up to the
    # fabricd pod-ready budget of 180s (client bound 200s), exec runs under
    # the runner's own 30s deadline (client bound 60s), delete gets a
    # bounded 120s for positive teardown.
    api() {
      local method="$1" path="$2" max_time="$3" body="${4-}" raw
      local args=(-sS --show-error --connect-timeout 2 --max-time "$max_time" -X "$method")
      if [[ -n "$body" ]]; then
        args+=(-H 'content-type: application/json' -d "$body")
      fi
      if ! raw="$(curl "${args[@]}" \
        --cacert "$tls_dir/ca-bundle.pem" \
        --cert "$tls_dir/client-cert.pem" \
        --key "$tls_dir/client-key.pem" \
        -w $'\n%{http_code}' "$base$path")"; then
        printf 'fabric-smoke: transport error on %s %s\n%s\n' "$method" "$path" "$raw" >&2
        return 1
      fi
      HTTP_CODE="${raw##*$'\n'}"
      HTTP_BODY="${raw%$'\n'*}"
      printf '%s %s -> HTTP %s\n' "$method" "$path" "$HTTP_CODE"
    }
    expect_http() { # STAGE EXPECTED_CODE
      if [[ "$HTTP_CODE" != "$2" ]]; then
        printf 'fabric-smoke: stage "%s" FAILED: HTTP %s (expected %s)\nresponse body:\n%s\n' "$1" "$HTTP_CODE" "$2" "$HTTP_BODY" >&2
        exit 1
      fi
    }
    expect_field() { # STAGE JQ_FILTER EXPECTED
      local got
      got="$(printf '%s' "$HTTP_BODY" | jq -r "$2" 2>&1)" || {
        printf 'fabric-smoke: stage "%s" FAILED: response unparsable at %s\nresponse body:\n%s\n' "$1" "$2" "$HTTP_BODY" >&2
        exit 1
      }
      if [[ -z "$got" || "$got" == null ]]; then
        printf 'fabric-smoke: stage "%s" FAILED: field %s absent\nresponse body:\n%s\n' "$1" "$2" "$HTTP_BODY" >&2
        exit 1
      fi
      if [[ "$got" != "$3" ]]; then
        printf 'fabric-smoke: stage "%s" FAILED: %s = <%s> (expected <%s>)\nresponse body:\n%s\n' "$1" "$2" "$got" "$3" "$HTTP_BODY" >&2
        exit 1
      fi
    }
    # The server resolves every dispatch attempt into exactly one honest
    # verdict: `terminal` with exit code/stdout/stderr, or `unknown` when
    # the attempt cannot be attributed to the program (no-replay). An
    # unknown verdict fails its named stage with the full response as
    # evidence; it is never masked, retried, or rewritten to terminal.
    expect_exec_terminal() { # STAGE
      expect_http "$1" 200
      local state
      state="$(printf '%s' "$HTTP_BODY" | jq -r '.state // ""' 2>/dev/null)" || state=""
      case "$state" in
        terminal) ;;
        unknown)
          printf 'fabric-smoke: stage "%s" FAILED: exec verdict is unknown (attempt not attributable to the program)\nresponse body:\n%s\n' "$1" "$HTTP_BODY" >&2
          exit 1
          ;;
        *)
          printf 'fabric-smoke: stage "%s" FAILED: unexpected exec state <%s>\nresponse body:\n%s\n' "$1" "$state" "$HTTP_BODY" >&2
          exit 1
          ;;
      esac
    }

    stage 'create-workspace'
    workspace_id="$(python3 -c 'import uuid; print(uuid.uuid4())')"
    api PUT "/v1/workspaces/${workspace_id}" 200 '{"revision":1,"desired":"active","runtimeProfile":"workspace-v1","volumeBytes":0}'
    expect_http create-workspace-put 200
    ready=0
    for _ in $(seq 1 90); do
      api GET "/v1/workspaces/${workspace_id}" 30
      expect_http create-workspace-get 200
      state="$(printf '%s' "$HTTP_BODY" | jq -r '.state // ""')"
      if [[ "$state" == "ready" || "$state" == "active" ]]; then
        ready=1
        break
      fi
      sleep 2
    done
    if [[ "$ready" != "1" ]]; then
      printf 'fabric-smoke: stage "create-workspace" FAILED: workspace did not become ready\nresponse body:\n%s\n' "$HTTP_BODY" >&2
      exit 1
    fi
    expect_field create-workspace .state ready
    expect_field create-workspace .runtime_class voie-firecracker
    expect_field create-workspace .generation 1
    if [[ -z "${workspace_id:-}" ]]; then
      printf 'fabric-smoke: stage "create-workspace" FAILED: no workspace id in response\nresponse body:\n%s\n' "$HTTP_BODY" >&2
      exit 1
    fi

    stage 'exec-e1-write-marker'
    write_body="$(jq -cn \
      --arg call_id dev-fabric-e1 \
      --arg command 'printf marker > /workspace/marker' \
      '{call_id: $call_id, command: $command}')"
    api POST "/v1/workspaces/$workspace_id/exec" 60 "$write_body"
    expect_exec_terminal exec-e1-write-marker
    expect_field exec-e1-write-marker .exit_code 0

    stage 'replace-workspace'
    api POST "/v1/workspaces/$workspace_id/replace" 200
    expect_http replace-workspace 200
    expect_field replace-workspace .state ready
    expect_field replace-workspace .generation 2

    stage 'exec-e2-read-marker'
    read_body="$(jq -cn \
      --arg call_id dev-fabric-e2 \
      --arg command 'cat /workspace/marker' \
      '{call_id: $call_id, command: $command}')"
    api POST "/v1/workspaces/$workspace_id/exec" 60 "$read_body"
    expect_exec_terminal exec-e2-read-marker
    expect_field exec-e2-read-marker .exit_code 0
    expect_field exec-e2-read-marker .stdout marker

    stage 'delete-workspace'
    api DELETE "/v1/workspaces/$workspace_id" 120
    expect_http delete-workspace 200
    expect_field delete-workspace .state deleted
    expect_field delete-workspace .cleaned.pod true
    expect_field delete-workspace .cleaned.reservation true
    expect_field delete-workspace .cleaned.jail true
    expect_field delete-workspace .cleaned.vmm true
    expect_field delete-workspace .cleaned.children true
    printf 'local Fabric smoke passed: C1 runtime identity and C2 workspace replacement\n'

dev-fabric-smoke-scoped:
    @bash dev-stack/run-scoped.sh fabric

dev-fabric-build-scoped:
    @bash dev-stack/run-scoped.sh fabric-build

web-smoke:
    pnpm --dir web install --frozen-lockfile
    pnpm --dir web run typecheck
    pnpm --dir web build

repo-hygiene:
    @tracked="$(git ls-files -- '.github/workflows/**' 'scripts/**' '*.tfvars' '*.tfvars.json' '.env' '.env.*' 'inventory/**' 'inventories/**' 'host_vars/**' 'group_vars/**' '*.age' '*.sops.*' '*.vault')"; if [ -n "$tracked" ]; then printf 'forbidden tracked deployment or secret material:\n%s\n' "$tracked" >&2; exit 1; fi

estate-check:
    #!/usr/bin/env bash
    set -euo pipefail
    tofu -chdir=infra/tofu/bootstrap fmt -check
    tofu -chdir=infra/tofu/bootstrap init -backend=false -input=false
    tofu -chdir=infra/tofu/bootstrap validate
    tofu -chdir=infra/tofu/r0 fmt -check
    r0_tf_data="$(mktemp -d)"
    trap 'rm -rf "$r0_tf_data"' EXIT
    TF_DATA_DIR="$r0_tf_data" tofu -chdir=infra/tofu/r0 init -backend=false -input=false
    TF_DATA_DIR="$r0_tf_data" tofu -chdir=infra/tofu/r0 validate
    ansible-galaxy collection install -r ansible/requirements.yml
    ansible-playbook -i control,fabric,localhost, -e ansible_connection=local --syntax-check ansible/control.yml
    ansible-playbook -i control,fabric,localhost, -e ansible_connection=local --syntax-check ansible/fabric.yml
    ansible-playbook -i control,fabric,localhost, -e ansible_connection=local --syntax-check ansible/verify.yml

live-c7:
    #!/usr/bin/env bash
    set -euo pipefail
    # Shared parser treats the caller-supplied env file strictly as data.
    # It allowlists deployment names and never evaluates shell fragments.
    # shellcheck source=tests/live/deploy-env.sh
    source tests/live/deploy-env.sh
    if [[ -n "${VOIE_C7_ENV_FILE:-}" ]]; then
      if [[ ! -r "${VOIE_C7_ENV_FILE}" ]]; then
        printf 'just live-c7: VOIE_C7_ENV_FILE is unreadable\n' >&2
        exit 2
      fi
      load_voie_deploy_env "${VOIE_C7_ENV_FILE}"
    fi
    normalize_voie_deploy_env
    load_voie_backend_cache "infra/tofu/r0/.terraform/terraform.tfstate"
    backend_from_cache="$VOIE_BACKEND_FROM_CACHE"
    discover_voie_workspace_pv
    required=(
      VOIE_SUBSCRIPTION_ID
      VOIE_TENANT_ID
      VOIE_BASE_DOMAIN
      CLOUDFLARE_ZONE_ID
      CLOUDFLARE_API_TOKEN
      VOIE_TF_BACKEND_RESOURCE_GROUP
      VOIE_TF_BACKEND_STORAGE_ACCOUNT
      VOIE_TF_BACKEND_CONTAINER
      VOIE_FABRIC_BOOTSTRAP_HOST
      VOIE_WORKSPACE_PV_DEVICE
      VOIE_ACME_EMAIL
    )
    missing=()
    for name in "${required[@]}"; do
      if [[ -z "${!name:-}" ]]; then
        missing+=("$name")
      fi
    done
    if [[ -z "${VOIE_BOOTSTRAP_ADMIN_USERNAME:-}" ]]; then
      missing+=("VOIE_BOOTSTRAP_ADMIN_USERNAME native admin bootstrap username")
    fi
    if [[ -z "${ARM_CLIENT_ID:-}" && -z "${ARM_ACCESS_TOKEN:-}" ]]; then
      if ! az account show >/dev/null 2>&1; then
        missing+=("Azure authentication (ARM_CLIENT_ID/ARM_CLIENT_SECRET or az login)")
      fi
    fi
    if ((${#missing[@]} > 0)); then
      printf 'just live-c7: live Azure/baremetal inputs are missing:\n' >&2
      printf '  %s\n' "${missing[@]}" >&2
      exit 2
    fi
    runtime_root="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    mkdir -p "$runtime_root"
    workdir="$(mktemp -d "$runtime_root/voie-r0.XXXXXX")"
    chmod 0700 "$workdir"
    cleanup() { rm -rf "$workdir"; }
    trap cleanup EXIT
    if [[ -z "${VOIE_MANAGEMENT_CIDRS:-}" ]]; then
      VOIE_MANAGEMENT_CIDRS="$(curl -fsS https://api.ipify.org)/32"
    fi
    # Fabric identity is resolved after managed state is readable.
    # The storage listKeys call is WAF-blocked for the deploy identity. The
    # operator supplies a complete backend fragment (VOIE_TF_BACKEND_HCL);
    # when present it IS the backend config. Secret values stay in the
    # operator's file and never enter the env, logs, or repo.
    if [[ -n "${VOIE_TF_BACKEND_HCL:-}" ]]; then
      if [[ ! -r "${VOIE_TF_BACKEND_HCL}" ]]; then
        printf 'just live-c7: VOIE_TF_BACKEND_HCL is unreadable\n' >&2
        exit 2
      fi
      cp "${VOIE_TF_BACKEND_HCL}" "$workdir/backend.hcl"
    else
      cat > "$workdir/backend.hcl" <<EOF
    resource_group_name  = "${VOIE_TF_BACKEND_RESOURCE_GROUP}"
    storage_account_name = "${VOIE_TF_BACKEND_STORAGE_ACCOUNT}"
    container_name       = "${VOIE_TF_BACKEND_CONTAINER}"
    key                  = "voie-r0.tfstate"
    EOF
    fi
    tofu -chdir=infra/tofu/r0 init -input=false -reconfigure -backend-config="$workdir/backend.hcl"
    prior_outputs="$workdir/prior-outputs.json"
    if ! tofu -chdir=infra/tofu/r0 output -json > "$prior_outputs"; then
      printf '{}\n' > "$prior_outputs"
    fi
    prior_state="$workdir/prior-state.json"
    if ! tofu -chdir=infra/tofu/r0 show -json > "$prior_state"; then
      printf '{}\n' > "$prior_state"
    fi
    resolve_voie_fabric_uuid "$prior_state"
    if [[ "$backend_from_cache" == "1" ]]; then
      state_base_domain="$(jq -r '.base_domain.value // empty' "$prior_outputs")"
      if [[ -z "$state_base_domain" || "$state_base_domain" != "$VOIE_BASE_DOMAIN" ]]; then
        printf 'just live-c7: cached backend does not prove the requested estate identity\n' >&2
        exit 2
      fi
    fi
    if [[ -z "${VOIE_CONTROL_IMAGE_ID:-}" && -z "${VOIE_CONTROL_IMAGE_VHD_PATH:-}" ]]; then
      state_has_vhd="$(jq -r '[.values.root_module.resources[]? | select(.type == "azurerm_storage_blob" and .name == "control_vhd")] | length > 0' "$prior_state")"
      state_external_image="$(jq -r '[.values.root_module.resources[]? | select(.type == "azurerm_linux_virtual_machine" and .name == "control") | .values.source_image_id] | if length == 1 then .[0] // empty else empty end' "$prior_state")"
      if [[ "$state_has_vhd" == "true" || -z "$state_external_image" ]]; then
        printf 'just live-c7: building collector Azure VHD input\n' >&2
        azure_image_out="$(nix build --no-link --print-out-paths .#nixosConfigurations.control-azure-image.config.system.build.azureImage)"
        azure_image_name="$(nix eval --raw .#nixosConfigurations.control-azure-image.config.image.fileName)"
        VOIE_CONTROL_IMAGE_VHD_PATH="$azure_image_out/$azure_image_name"
        if [[ ! -f "$VOIE_CONTROL_IMAGE_VHD_PATH" ]]; then
          printf 'just live-c7: collector Azure image output has no VHD payload\n' >&2
          exit 2
        fi
      else
        VOIE_CONTROL_IMAGE_ID="$state_external_image"
      fi
    fi
    if [[ -z "${VOIE_LOCATION:-}" ]]; then
      VOIE_LOCATION="$(jq -r '[.values.root_module.resources[]? | select(.address == "azurerm_resource_group.r0") | .values.location] | if length == 1 then .[0] else empty end' "$prior_state")"
    fi
    # Azure admin_ssh_key is ForceNew. An injected agent key that differs
    # from the live VM must not replace the control node. Rotation is a
    # separate explicit operator action.
    state_admin_ssh="$(jq -r '[.values.root_module.resources[]? | select(.address == "azurerm_linux_virtual_machine.control[0]") | .values.admin_ssh_key[0].public_key] | .[0] // empty' "$prior_state")"
    if [[ -z "$state_admin_ssh" ]]; then
      state_admin_ssh="$(jq -r '[.values.root_module.resources[]? | select(.type == "azurerm_linux_virtual_machine" and .name == "control") | .values.admin_ssh_key[]?.public_key] | if length == 1 then .[0] else empty end' "$prior_state")"
    fi
    if [[ -n "$state_admin_ssh" ]]; then
      if [[ -n "${VOIE_ADMIN_SSH_PUBLIC_KEY:-}" && "$VOIE_ADMIN_SSH_PUBLIC_KEY" != "$state_admin_ssh" ]]; then
        printf 'just live-c7: keeping the existing control VM admin SSH key; Azure key rotation would replace the VM\n' >&2
      fi
      VOIE_ADMIN_SSH_PUBLIC_KEY="$state_admin_ssh"
    fi
    # Live boundary: the deployed estate runs native auth. OIDC provisioning
    # stays off unless the operator explicitly widens it with
    # VOIE_OIDC_PROVISION=true; the managed-state autodetect is gone so a
    # stale Entra registration can never re-enable it silently.
    if [[ -z "${VOIE_OIDC_PROVISION:-}" ]]; then
      oidc_provision="false"
    else
      oidc_provision="${VOIE_OIDC_PROVISION,,}"
    fi
    case "$oidc_provision" in
      true|false) ;;
      *)
        printf 'just live-c7: VOIE_OIDC_PROVISION must be true or false\n' >&2
        exit 2
        ;;
    esac

    missing=()
    for name in VOIE_LOCATION VOIE_ADMIN_SSH_PUBLIC_KEY; do
      [[ -z "${!name:-}" ]] && missing+=("$name")
    done
    image_count=0
    [[ -n "${VOIE_CONTROL_IMAGE_ID:-}" ]] && ((image_count += 1))
    [[ -n "${VOIE_CONTROL_IMAGE_VHD_PATH:-}" ]] && ((image_count += 1))
    if ((image_count != 1)); then
      missing+=("exactly one of VOIE_CONTROL_IMAGE_ID or VOIE_CONTROL_IMAGE_VHD_PATH")
    fi
    if ((${#missing[@]} > 0)); then
      printf 'just live-c7: current managed state could not derive required deployment inputs:\n' >&2
      printf '  %s\n' "${missing[@]}" >&2
      exit 2
    fi
    # Rewrite the input after all nonsecret managed-state derivations. This
    # guard keeps Terraform's control VM count from falling to zero when an
    # image input was omitted by the caller.
    jq -n \
      --arg subscription_id "$VOIE_SUBSCRIPTION_ID" \
      --arg tenant_id "$VOIE_TENANT_ID" \
      --arg location "$VOIE_LOCATION" \
      --arg control_image_id "${VOIE_CONTROL_IMAGE_ID:-}" \
      --arg control_image_vhd_path "${VOIE_CONTROL_IMAGE_VHD_PATH:-}" \
      --arg admin_ssh_public_key "$VOIE_ADMIN_SSH_PUBLIC_KEY" \
      --arg cloudflare_zone_id "$CLOUDFLARE_ZONE_ID" \
      --arg base_domain "$VOIE_BASE_DOMAIN" \
      --arg public_hostname "${VOIE_PUBLIC_HOSTNAME:-}" \
      --arg management_cidrs "$VOIE_MANAGEMENT_CIDRS" \
      --arg oidc_provision "$oidc_provision" \
      '{
        subscription_id: $subscription_id,
        tenant_id: $tenant_id,
        location: $location,
        control_image_id: $control_image_id,
        control_image_vhd_path: $control_image_vhd_path,
        admin_ssh_public_key: $admin_ssh_public_key,
        cloudflare_zone_id: $cloudflare_zone_id,
        cloudflare_api_token: env.CLOUDFLARE_API_TOKEN,
        base_domain: $base_domain,
        public_hostname: $public_hostname,
        management_cidrs: ($management_cidrs | split(",") | map(select(length > 0))),
        oidc_provision: ($oidc_provision == "true")
      }' > "$workdir/r0.tfvars.json"
    # Destructive-replacement gate: plan to a file, inspect the action
    # classes, refuse replace/destroy on protected families, then apply the
    # saved plan. The operator may force a full wipe with VOIE_C7_WIPE=1.
    tofu -chdir=infra/tofu/r0 plan -input=false -parallelism=2 -out="$workdir/r0.plan" -var-file="$workdir/r0.tfvars.json"
    # Always inspect the plan: pipe the JSON straight to jq (never retained
    # in a variable). WIPE only waives the refusal, not the inspection.
    destroy_count="$(tofu -chdir=infra/tofu/r0 show -json "$workdir/r0.plan" | jq -c '[.resource_changes[]? | .change.actions | select(.[0] == "delete")] | length')"
    replace_count="$(tofu -chdir=infra/tofu/r0 show -json "$workdir/r0.plan" | jq -c '[.resource_changes[]? | .change.actions | select(.[0] == "delete" and .[1] == "create")] | length')"
    printf 'just live-c7: plan gate: destroy=%s replace=%s\n' "$destroy_count" "$replace_count"
    if [[ "${VOIE_C7_WIPE:-0}" != "1" && "$destroy_count" != "0" ]]; then
      printf 'just live-c7: refusing destructive apply; set VOIE_C7_WIPE=1 to allow the full wipe\n' >&2
      exit 2
    fi
    tofu -chdir=infra/tofu/r0 apply -input=false -auto-approve -parallelism=2 "$workdir/r0.plan"
    tofu -chdir=infra/tofu/r0 output -json > "$workdir/outputs.json"
    # Native admin bootstrap: the username is a nonsecret extra var; the
    # password is provisioned by OpenTofu into Key Vault and delivered by
    # Ansible to a root:voie-cloud 0640 file. During converge it never enters
    # control.env, extra-vars, or argv; the acceptance handoff stages it only
    # in a temporary 0600 file.
    # OIDC: when provisioning is disabled the outputs are null and the oidc_*
    # keys are omitted entirely, so Ansible renders no OIDC configuration.
    jq -n \
      --slurpfile tofu "$workdir/outputs.json" \
      --arg workspace_pv_device "$VOIE_WORKSPACE_PV_DEVICE" \
      --arg acme_email "$VOIE_ACME_EMAIL" \
      --arg fabric_uuid "$VOIE_FABRIC_UUID" \
      --arg ansible_user "${VOIE_SSH_USER:-voie}" \
      --arg native_admin_username "${VOIE_BOOTSTRAP_ADMIN_USERNAME:-}" \
      --arg cloudflare_zone_id "${CLOUDFLARE_ZONE_ID:-}" \
      '($tofu[0]) as $tofu | {
        postgres_fqdn: $tofu.postgres_fqdn.value,
        control_identity_client_id: $tofu.control_identity_client_id.value,
        blob_account_name: $tofu.blob_account_name.value,
        blob_container_name: $tofu.blob_container_name.value,
        blob_endpoint: $tofu.blob_endpoint.value,
        key_vault_uri: $tofu.key_vault_uri.value,
        user_secrets_key_vault_uri: $tofu.user_secrets_key_vault_uri.value,
        public_hostname: $tofu.public_hostname.value,
        headscale_hostname: $tofu.headscale_hostname.value,
        control_public_ip: $tofu.control_public_ip.value,
        workspace_pv_device: $workspace_pv_device,
        acme_email: $acme_email,
        fabric_uuid: $fabric_uuid,
        ansible_user: $ansible_user,
        cloudflare_zone_id: $cloudflare_zone_id
      }
      + (if env.CLOUDFLARE_API_TOKEN != "" then
           { cloudflare_api_token: env.CLOUDFLARE_API_TOKEN }
         else {} end)
      + (if $native_admin_username != "" then
           { native_admin_username: $native_admin_username }
         else {} end)
      + (if env.VOIE_WIPE_VOIE_WS == "DESTROY" then
           { voie_wipe_voie_ws: "DESTROY" }
         else {} end)
      + (if $tofu.oidc_client_id.value != null then
           { oidc_issuer: $tofu.oidc_issuer.value,
             oidc_client_id: $tofu.oidc_client_id.value,
             oidc_client_secret: $tofu.oidc_client_secret.value }
         else {} end)
      + (if env.VOIE_MODEL_BASE_URL != "" then
           { model_base_url: env.VOIE_MODEL_BASE_URL }
         else {} end)
      + (if env.VOIE_MODEL_NAME != "" then
           { model_name: env.VOIE_MODEL_NAME }
         else {} end)
      + (if env.VOIE_MODEL_API_KEY != "" then
           { model_api_key: env.VOIE_MODEL_API_KEY }
         else {} end)' > "$workdir/extra-vars.json"
    ssh_key_opt=""
    if [[ -n "${VOIE_SSH_PRIVATE_KEY:-}" ]]; then
      ssh_key_opt=" ansible_ssh_private_key_file=${VOIE_SSH_PRIVATE_KEY}"
    fi
    control_ip="$(jq -r '.control_public_ip.value // empty' "$workdir/outputs.json")"
    if [[ -z "$control_ip" ]]; then
      printf 'just live-c7: current state has no control public IP\n' >&2
      exit 2
    fi
    {
      printf '[control]\n'
      printf 'control ansible_host=%s ansible_user=%s%s\n' "$control_ip" "${VOIE_SSH_USER:-voie}" "$ssh_key_opt"
      printf '\n[fabric]\n'
      printf 'fabric ansible_host=%s ansible_user=%s%s\n' "$VOIE_FABRIC_BOOTSTRAP_HOST" "${VOIE_SSH_USER:-voie}" "$ssh_key_opt"
    } > "$workdir/inventory.ini"
    export ANSIBLE_CONFIG="$PWD/ansible/ansible.cfg"
    ansible-galaxy collection install -r ansible/requirements.yml
    # The bootstrap admin password is not an extra var: OpenTofu provisions
    # it into Key Vault and this play reads it with the controller token. During
    # converge no secret value enters extra-vars or argv; only the 0600 path
    # is passed to the control play.
    bootstrap_password_file="$workdir/bootstrap-admin-password"
    if [[ "${VOIE_BOOTSTRAP_ADMIN_PASSWORD:-}" == *VOIE_TF_BACKEND_ACCESS_KEY=* ]]; then
      printf 'just live-c7: VOIE_BOOTSTRAP_ADMIN_PASSWORD contains concatenated env, refusing\n' >&2
      exit 2
    fi
    if [[ -n "${VOIE_BOOTSTRAP_ADMIN_PASSWORD:-}" ]]; then
      (umask 077; printf '%s\n' "$VOIE_BOOTSTRAP_ADMIN_PASSWORD" > "$bootstrap_password_file")
    elif [[ -n "${VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE:-}" ]]; then
      cp "$VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE" "$bootstrap_password_file"
    else
      (umask 077; az keyvault secret show --subscription "$VOIE_SUBSCRIPTION_ID" --vault-name "$key_vault_name" --name voie-bootstrap-admin-password --query value -o tsv > "$bootstrap_password_file")
    fi
    chmod 0600 "$bootstrap_password_file"
    if grep -q 'VOIE_TF_BACKEND_ACCESS_KEY=' "$bootstrap_password_file"; then
      printf 'just live-c7: bootstrap admin password file contains concatenated env, refusing\n' >&2
      exit 2
    fi
    ansible-playbook -i "$workdir/inventory.ini" -e @"$workdir/extra-vars.json" -e "bootstrap_password_file=$bootstrap_password_file" ansible/control.yml
    ansible-playbook -i "$workdir/inventory.ini" -e @"$workdir/extra-vars.json" ansible/fabric.yml
    # Fabric join is what publishes the Headscale IPv4; re-run control so
    # the NixOS hosts overlay can pin DNS:baremetal-1 for fabric mTLS.
    # Later control converges keep extra-vars.json; they must not DESTROY
    # the schema the first converge just remigrated.
    ansible-playbook -i "$workdir/inventory.ini" -e @"$workdir/extra-vars.json" -e "bootstrap_password_file=$bootstrap_password_file" -e voie_wipe_voie_ws=none ansible/control.yml
    fabric_ssh="${VOIE_FABRIC_SSH:-${VOIE_FABRIC_BOOTSTRAP_HOST}}"
    control_ssh="${VOIE_CONTROL_SSH:-control}"
    # Persistent NixOS generation: copy closure, set /nix/var/nix/profiles/system,
    # switch-to-configuration switch, prove /run/current-system.
    bash tests/live/switch-generation.sh "$fabric_ssh" .#nixosConfigurations.fabric.config.system.build.toplevel voie-fabricd
    bash tests/live/switch-generation.sh "$control_ssh" .#nixosConfigurations.control.config.system.build.toplevel voie-cloud
    # Second converge: gateway control IP, rescue disarm, images/config.
    ansible-playbook -i "$workdir/inventory.ini" -e @"$workdir/extra-vars.json" -e voie_final_converge=true -e voie_wipe_voie_ws=none ansible/fabric.yml
    ansible-playbook -i "$workdir/inventory.ini" -e @"$workdir/extra-vars.json" -e "bootstrap_password_file=$bootstrap_password_file" -e voie_wipe_voie_ws=none ansible/control.yml
    ssh -o BatchMode=yes -o ConnectTimeout=8 "$fabric_ssh" 'systemctl restart voie-fabricd'
    ssh -o BatchMode=yes -o ConnectTimeout=8 "$control_ssh" 'systemctl restart voie-cloud'
    public_hostname="$(jq -r '.public_hostname.value // empty' "$workdir/outputs.json")"
    if [[ -z "$public_hostname" ]]; then
      printf 'just live-c7: current state has no public hostname\n' >&2
      exit 2
    fi
    export VOIE_CONTROL_URL="https://${public_hostname}"
    export VOIE_BOOTSTRAP_ADMIN_USERNAME
    export VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE="$bootstrap_password_file"
    bash tests/live/wait-database-security.sh
    ansible-playbook -i "$workdir/inventory.ini" -e @"$workdir/extra-vars.json" ansible/verify.yml
    key_vault_name="$(jq -r '.key_vault_name.value // empty' "$workdir/outputs.json")"
    if [[ -z "$key_vault_name" ]]; then
      printf 'just live-c7: current state has no Key Vault name\n' >&2
      exit 2
    fi
    bash tests/live/native-c6.sh
    echo "live-c7 pass: collector estate reproduced native C6; persistent NixOS generations switched; configured operator management retained"


live-c7-proof:
    #!/usr/bin/env bash
    set -euo pipefail
    origin="${VOIE_CONTROL_URL:-${VOIE_C7_ORIGIN:-}}"
    case "$origin" in
      https://*) ;;
      *)
        printf 'just live-c7-proof: set VOIE_CONTROL_URL or VOIE_C7_ORIGIN to the deployed https:// origin\n' >&2
        exit 2
        ;;
    esac
    if [[ -z "${VOIE_BOOTSTRAP_ADMIN_USERNAME:-}" ]]; then
      printf 'just live-c7-proof: set VOIE_BOOTSTRAP_ADMIN_USERNAME to the deployed bootstrap admin username\n' >&2
      exit 2
    fi
    if [[ -z "${VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE:-}" ]]; then
      printf 'just live-c7-proof: set VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE to a 0600 file holding the bootstrap admin password\n' >&2
      exit 2
    fi
    if [[ ! -r "${VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE}" ]]; then
      printf 'just live-c7-proof: VOIE_BOOTSTRAP_ADMIN_PASSWORD_FILE is unreadable\n' >&2
      exit 2
    fi
    export VOIE_CONTROL_URL="${origin%/}"
    bash tests/live/native-c6.sh
    echo "live-c7-proof pass: collector estate reproduced native C6"

# C8 is the full live recovery/reboot checkpoint. Configured operator SSH is
# intentionally persistent and must remain usable; this recipe never changes
# management_cidrs or management exposure.
live-c8:
    #!/usr/bin/env bash
    set -euo pipefail
    # shellcheck source=tests/live/deploy-env.sh
    source tests/live/deploy-env.sh
    if [[ -n "${VOIE_C8_ENV_FILE:-${VOIE_C7_ENV_FILE:-}}" ]]; then
      env_file_name="VOIE_C7_ENV_FILE"
      if [[ -n "${VOIE_C8_ENV_FILE:-}" ]]; then
        env_file_name="VOIE_C8_ENV_FILE"
      fi
      env_file="${VOIE_C8_ENV_FILE:-${VOIE_C7_ENV_FILE}}"
      if [[ ! -r "$env_file" ]]; then
        printf 'just live-c8: %s is unreadable\n' "$env_file_name" >&2
        exit 2
      fi
      load_voie_deploy_env "$env_file"
    fi
    normalize_voie_deploy_env
    load_voie_backend_cache "infra/tofu/r0/.terraform/terraform.tfstate"
    VOIE_C8_CONFIRM="${VOIE_C8_CONFIRM:-yes}" bash tests/live/c8.sh
    echo "live-c8 pass: isolation, unknown/no-replay, recovery, restore, cleanup, and configured operator SSH recovery proven"

# Local declarative cloud stack for development: real PostgreSQL plus real
# Azure Blob semantics from the default Floci-AZ boundary (the documented
# floci/floci-az:latest rootless Podman image; VOIE_DEV_CLOUD_EMULATOR=azurite
# is the explicit fallback only). Launched through the shared capped
# voie-dev-stack.slice so every child process stays bounded.
dev-cloud-up:
    @bash dev-stack/run-scoped.sh cloud up

dev-cloud-down:
    @bash dev-cloud/local-stack.sh down

dev-cloud-env:
    @bash dev-cloud/local-stack.sh env

# Explicit one-time provisioning of the default Floci-AZ boundary inside the
# bounded resource domain: verifies host Podman support and pulls the image
# once. Never triggered by dev-cloud-up or dev-stack-up; both fail closed
# pointing here when Podman or the image is missing.
dev-cloud-provision:
    @bash dev-stack/run-scoped.sh cloud provision

# Acceptance run against the running cloud. Safe while `dev-cloud-up` stays
# up: each operation gets its own child scope under voie-dev-stack.slice,
# and the slice's ceilings (MemoryMax=8G, MemorySwapMax=2G, TasksMax,
# CPUQuota) bound the SUM of all members, never each operation separately.
dev-cloud-check:
    @bash dev-stack/run-scoped.sh cloud check

# Full local development stack (cloud data plane + KVM Fabric VM + ephemeral
# OIDC issuer + built Web artifact + one voie-cloud process), launched
# as a per-operation child scope of the fail-closed systemd user slice
# voie-dev-stack.slice (MemoryMax=8G, MemorySwapMax=2G, TasksMax,
# CPUQuota). The slice enforces its ceilings over the sum of all member
# cgroups, so concurrent operations stay within one aggregate cap while
# covering every child: Nix/Rust builds, QEMU, cloud services, OIDC,
# emulators.
dev-stack-up:
    @bash dev-stack/run-scoped.sh up

# Ordered local acceptance gates inside the same resource domain.
dev-stack-check:
    @bash dev-stack/run-scoped.sh check

# One-time developer provisioning: package installs and frontend builds,
# outside stack startup; `up` only consumes the produced artifacts.
dev-stack-provision:
    @bash dev-stack/run-scoped.sh provision

# Read-only report of the resource domain: slice limits, member operation
# scopes, member PIDs.
dev-stack-report:
    @bash dev-stack/check.sh --report

# Stop every operation scope and the capped slice (SIGTERM to every
# remaining stack child) and remove only the stack's XDG_RUNTIME_DIR state.
# Runs outside the resource domain by design.
dev-stack-down:
    @bash dev-stack/down.sh


# Integration entry points: thin aliases only. dev-up goes through the same
# fail-closed voie-dev-stack.slice scope (run-scoped.sh keeps the guard and
# caps); dev-down uses the same safe teardown path as dev-stack-down.
dev-up:
    @bash dev-stack/run-scoped.sh up

dev-down:
    @bash dev-stack/down.sh

# Run the headless browser smoke against VOIE_SMOKE_ORIGIN (see tests/browser/README.md).
# ensure-chromium.sh pins Chrome-for-Testing headless-shell when the system
# Chrome is 148+ (CDP Page.navigate never commits with --remote-debugging-port).
browser-smoke *args:
    @nix develop -c bash -c 'exe="$(command -v chrome-headless-shell)"; test -x "$exe" || { printf "just browser-smoke: nix-pinned chrome-headless-shell is missing from PATH\n" >&2; exit 2; }; export VOIE_SMOKE_EXECUTABLE="$exe"; exec node tests/browser/steps.mjs "$@"' -- {{args}}

# Product todo E2E against the real deployed stack. Fresh Workspace create
# is mandatory; 429 is a failure. See tests/browser/e2e-todo.mjs.
e2e-todo *args:
    @nix develop -c bash -c 'exe="$(command -v chrome-headless-shell)"; test -x "$exe" || { printf "just e2e-todo: nix-pinned chrome-headless-shell is missing from PATH\n" >&2; exit 2; }; export VOIE_SMOKE_EXECUTABLE="$exe"; exec node tests/browser/e2e-todo.mjs "$@"' -- {{args}}

# Browser Team lifecycle against VOIE_SMOKE_ORIGIN. Create team, add member,
# refuse Owner promotion, revoke access. See tests/browser/e2e-team.mjs.
e2e-team *args:
    @nix develop -c bash -c 'exe="$(command -v chrome-headless-shell)"; test -x "$exe" || { printf "just e2e-team: nix-pinned chrome-headless-shell is missing from PATH\n" >&2; exit 2; }; export VOIE_SMOKE_EXECUTABLE="$exe"; exec node tests/browser/e2e-team.mjs "$@"' -- {{args}}
