#!/usr/bin/env bash
# Source-only safe-teardown core shared by dev-stack/down.sh and
# dev-stack/up.sh.
#
# Contract: every termination goes through either a systemd unit stop or an
# identity-validated pid_guard_stop signal. Nothing is ever killed by raw
# PID scan. "Stopped" is never assumed from the stop call itself; death is
# verified against the manager (ActiveState) and, at the end, against the
# live cgroup tree. stack_teardown returns 0 ONLY when no owned process
# remains outside the caller's own scope, so callers may print success or
# start a fresh stack on that basis.

# Locate the slice's live cgroup directory via the manager itself, so the
# check works under any user-slice hierarchy (e.g. .../voie.slice/voie-dev
# .slice/voie-dev-stack.slice) instead of assuming a fixed path.
stack_slice_cgroup_dir() {
  local slice_name="$1"
  systemctl --user show -P ControlGroup "$slice_name" 2>/dev/null
}

# Stop one unit and verify its death through the manager. Query failures are
# failures: an unreachable or silent manager can never prove a process gone,
# and treating "cannot ask" as "stopped" is exactly how a previous revision
# reported success over live survivors.
stack_stop_unit_verified() {
  local unit="$1" label="${2:-$1}" state i
  systemctl --user stop "$unit" 2>/dev/null || true
  for _ in $(seq 1 60); do
    state="$(systemctl --user show -P ActiveState "$unit" 2>/dev/null)" ||
      { printf 'stack-teardown: cannot query %s state; refusing to claim it stopped\n' "$label" >&2; return 1; }
    case "$state" in
      inactive | failed | unknown | "") return 0 ;;
    esac
    sleep 0.5
  done
  printf 'stack-teardown: %s still %s after 30s\n' "$label" "${state:-unknown}" >&2
  return 1
}

# Enumerate the transient per-operation scopes that belong to the stack:
# everything systemd lists plus anything visible as a cgroup child of the
# slice (covers units list lag right after creation). One scope name per
# line, deduplicated.
stack_owned_scopes() {
  local slice_name="$1" slice_dir scope_dir
  {
    systemctl --user list-units --all --plain --no-legend \
      "${slice_name%.slice}-*.scope" 2>/dev/null | awk '{print $1}'
    slice_dir="$(stack_slice_cgroup_dir "$slice_name")"
    if [[ -n "$slice_dir" && -d "$slice_dir" ]]; then
      while IFS= read -r scope_dir; do
        basename "$scope_dir"
      done < <(find "$slice_dir" -mindepth 1 -maxdepth 1 -type d -name '*.scope' 2>/dev/null)
    fi
  } | awk '!(seen[$0]++)'
}

# Final proof: walk every cgroup.procs under the slice and fail when any
# process lives outside the caller's own scope subtree. No signals here —
# this is verification only.
stack_verify_slice_empty() {
  local slice_name="$1" self_scope="$2" slice_dir procs pid cg remaining=0
  slice_dir="$(stack_slice_cgroup_dir "$slice_name")"
  if [[ -z "$slice_dir" || ! -d "$slice_dir" ]]; then
    return 0 # slice cgroup gone entirely: nothing can be left inside it
  fi
  procs="$(find "$slice_dir" -name cgroup.procs -exec cat {} + 2>/dev/null || true)"
  for pid in $procs; do
    [[ "$pid" == "$$" ]] && continue
    if [[ -n "$self_scope" ]] &&
      cg="$(sed -n 's/^0:://p' "/proc/$pid/cgroup" 2>/dev/null)" &&
      [[ "$cg" == *"/$self_scope"* ]]; then
      continue # our own operation's subtree
    fi
    printf 'stack-teardown: survivor pid %s still inside %s: %s\n' \
      "$pid" "$slice_name" "$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | cut -c1-120)" >&2
    remaining=1
  done
  return "$remaining"
}

# Tear down every process the dev stack owns. Arguments:
#   $1 scope_prefix        e.g. voie-dev-stack
#   $2 runtime_root        .../voie-dev-stack      (caddy/model/cloud/oidc pids)
#   $3 fabric_runtime_root .../voie-fabric-dev     (qemu.pid)
#   $4 self_scope          scope name to leave untouched, "" for none
# Sourced scripts must already have pid-guard.sh available (both callers
# source it); load it here only when missing so this file stays standalone.
stack_teardown() {
  local scope_prefix="$1" runtime_root="$2" fabric_runtime_root="$3" self_scope="$4"
  local slice_name="${scope_prefix}.slice" legacy_scope="${scope_prefix}.scope"
  local unit ok=0

  declare -F pid_guard_stop >/dev/null ||
    source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/pid-guard.sh"

  # Children first, never the caller's own scope.
  while IFS= read -r unit; do
    [[ -n "$unit" && "$unit" != "$self_scope" ]] || continue
    stack_stop_unit_verified "$unit" || ok=1
  done < <(stack_owned_scopes "$slice_name")
  if [[ -n "$legacy_scope" && "$legacy_scope" != "$self_scope" ]] &&
    systemctl --user list-units --all --plain --no-legend "$legacy_scope" 2>/dev/null | grep -q .; then
    stack_stop_unit_verified "$legacy_scope" || ok=1
  fi

  # Identity-checked fallbacks: unscoped allow-listed daemons and the VM.
  local f base
  for f in caddy model-emulator cloud oidc; do
    base="$runtime_root/$f.pid"
    [[ -e "$base" ]] && { pid_guard_stop "$base" "$scope_prefix" || ok=1; }
  done
  if [[ -e "$fabric_runtime_root/qemu.pid" ]]; then
    pid_guard_stop "$fabric_runtime_root/qemu.pid" "$scope_prefix" || ok=1
  fi

  # The verdict that matters: the aggregate cap subtree holds nothing but,
  # at most, the caller's own scope.
  stack_verify_slice_empty "$slice_name" "$self_scope" || return 1
  return "$ok"
}
