#!/usr/bin/env bash
# Fail-closed launcher for the local development stack.
#
# Everything the stack spawns (Nix/Rust builds, QEMU, voie-cloud, the OIDC
# issuer, cloud data-plane children) must live inside ONE bounded systemd
# user resource domain so a runaway build or VM can never exhaust the
# workstation.
#
# The domain is the shared parent slice voie-dev-stack.slice. Every
# operation (up, check, provision, cloud) runs in its own transient child
# scope that joins that slice. Slice ceilings are enforced by the kernel
# over the SUM of all member cgroups, so concurrent operations — a running
# cloud stack plus `just dev-cloud-check`, for example — together can never
# exceed the single aggregate cap. No operation needs to wait for another
# to finish and none can escape the cap by starting early.
#
# This script verifies cgroup v2 and the user manager FIRST, then starts
# the slice and re-reads its live cgroupfs attributes BEFORE exec'ing any
# entry point; if the requested limits cannot be established it exits
# without starting any child process.
#
# Ceilings (environment-overridable; defaults are the supported contract):
#   VOIE_DEV_STACK_MEMORY_MAX   hard memory ceiling   (default 8G)
#   VOIE_DEV_STACK_SWAP_MAX     swap ceiling          (default 2G)
#   VOIE_DEV_STACK_TASKS_MAX    task/PID ceiling      (default 512)
#   VOIE_DEV_STACK_CPU_QUOTA    CPU quota             (default 200%)
#   VOIE_DEV_STACK_SCOPE        unit-name prefix      (default voie-dev-stack)
set -euo pipefail

scope_prefix="${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}"
slice_name="$scope_prefix.slice"
memory_max="${VOIE_DEV_STACK_MEMORY_MAX:-8G}"
swap_max="${VOIE_DEV_STACK_SWAP_MAX:-2G}"
tasks_max="${VOIE_DEV_STACK_TASKS_MAX:-512}"
cpu_quota="${VOIE_DEV_STACK_CPU_QUOTA:-200%}"

die() {
  printf 'dev-stack-scope: %s\n' "$*" >&2
  exit 2
}

if [[ ! "$scope_prefix" =~ ^voie-dev-stack(-[A-Za-z0-9]+)?$ ]]; then
  die "invalid VOIE_DEV_STACK_SCOPE; refusing to address an unrelated unit"
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# --- static limit math -------------------------------------------------
# Pure helpers over the declared contract; no side effects, safe to source.

memory_to_bytes() {
  local value="${1^^}" count unit
  value="${value// /}"
  [[ "$value" =~ ^([0-9]+)([KMG]?)$ ]] || return 1
  count="${BASH_REMATCH[1]}" unit="${BASH_REMATCH[2]}"
  case "$unit" in
    K) count="$((count * 1024))" ;;
    M) count="$((count * 1024 * 1024))" ;;
    G) count="$((count * 1024 * 1024 * 1024))" ;;
  esac
  printf '%s' "$count"
}

# Convert "<percent>%" into the cgroupfs cpu.max pair "<quota> <period>",
# where the period comes from the live cpu.max second field (usually
# 100000). The kernel enforces quota/period CPU time across the whole
# subtree, which is exactly the aggregate contract.
cpu_quota_to_cgroupfs() {
  local percent period
  [[ "$1" =~ ^([0-9]+)%$ ]] || return 1
  percent="${BASH_REMATCH[1]}"
  period="$(printf '%s' "${2:-}" | tr -dc '0-9')"
  [[ -n "$period" ]] && ((period > 0)) || return 1
  printf '%s %s' "$((percent * period / 100))" "$period"
}

verify_declared_limits() {
  local memory_bytes swap_bytes cpu_percent
  memory_bytes="$(memory_to_bytes "$memory_max")" ||
    die "invalid VOIE_DEV_STACK_MEMORY_MAX; refusing to start any child"
  swap_bytes="$(memory_to_bytes "$swap_max")" ||
    die "invalid VOIE_DEV_STACK_SWAP_MAX; refusing to start any child"
  [[ "$tasks_max" =~ ^[0-9]+$ ]] ||
    die "invalid VOIE_DEV_STACK_TASKS_MAX; refusing to start any child"
  [[ "$cpu_quota" =~ ^([0-9]+)%$ ]] ||
    die "invalid VOIE_DEV_STACK_CPU_QUOTA; refusing to start any child"
  cpu_percent="${BASH_REMATCH[1]}"
  (( memory_bytes > 0 && memory_bytes <= 8 * 1024 * 1024 * 1024 )) ||
    die "VOIE_DEV_STACK_MEMORY_MAX exceeds the hard 8G cap"
  (( swap_bytes >= 0 && swap_bytes <= 2 * 1024 * 1024 * 1024 )) ||
    die "VOIE_DEV_STACK_SWAP_MAX exceeds the hard 2G cap"
  (( tasks_max > 0 && tasks_max <= 512 )) ||
    die "VOIE_DEV_STACK_TASKS_MAX exceeds the hard 512-task cap"
  (( cpu_percent > 0 && cpu_percent <= 200 )) ||
    die "VOIE_DEV_STACK_CPU_QUOTA exceeds the hard 200% cap"
}

verify_prerequisites() {
  verify_declared_limits
  if ! test -r /sys/fs/cgroup/cgroup.controllers; then
    die "cgroup v2 unified hierarchy not available at /sys/fs/cgroup; cannot enforce resource limits, refusing to start any child"
  fi
  if ! command -v systemd-run >/dev/null 2>&1; then
    die "systemd-run not found in PATH; cannot enforce resource limits, refusing to start any child"
  fi
  if ! command -v systemctl >/dev/null 2>&1; then
    die "systemctl not found in PATH; cannot manage the stack resource domain"
  fi
  local state
  state="$(systemctl --user is-system-running 2>/dev/null || true)"
  case "$state" in
    running | degraded | maintenance) ;;
    *) die "systemd user manager not reachable (state: ${state:-unknown}); refusing to start any child" ;;
  esac
}

# True when this process already lives inside the bounded domain: either a
# per-operation child scope under the shared slice, or the legacy single
# fixed scope from earlier revisions (same ceilings).
inside_bounded_domain() {
  local cgroup_path
  cgroup_path="$(sed -n 's/^0:://p' /proc/self/cgroup)"
  case "$cgroup_path" in
    */"$slice_name"/* | */"$scope_prefix".scope) return 0 ;;
    *) return 1 ;;
  esac
}

user_unit_dir() {
  printf '%s/systemd/user' "${XDG_CONFIG_HOME:-$HOME/.config}"
}

write_slice_unit() {
  local dir file tmp
  dir="$(user_unit_dir)"
  mkdir -p "$dir"
  file="$dir/$slice_name"
  tmp="$(mktemp "$file.tmp.XXXXXX")"
  {
    printf '# Generated by dev-stack/run-scoped.sh. Aggregate cap for ALL local\n'
    printf '# stack operations (up/check/provision/cloud): every child scope joins\n'
    printf '# this slice, so these ceilings bound their SUM, not each operation.\n'
    printf '[Slice]\n'
    printf 'MemoryMax=%s\n' "$memory_max"
    printf 'MemorySwapMax=%s\n' "$swap_max"
    printf 'TasksMax=%s\n' "$tasks_max"
    printf 'CPUQuota=%s\n' "$cpu_quota"
  } >"$tmp"
  mv "$tmp" "$file"
}

# Locate the slice's live cgroupfs directory under the user manager's
# delegated subtree.
slice_cgroup_dir() {
  local base hit uid
  uid="$(id -u)"
  base="/sys/fs/cgroup/user.slice/user-$uid.slice/user@$uid.service"
  if test -d "$base/$slice_name"; then
    printf '%s' "$base/$slice_name"
    return 0
  fi
  hit="$(find "$base" -xdev -maxdepth 6 -type d -name "$slice_name" -print -quit 2>/dev/null || true)"
  test -n "$hit" || return 1
  printf '%s' "$hit"
}

require_attr_value() {
  local file="$1" expected="$2" label="$3" actual
  actual="$(cat "$file" 2>/dev/null)" ||
    die "cannot read $file; $label not proven, refusing to start any child"
  actual="${actual//$'\n'/}"
  [[ "$actual" == "$expected" ]] ||
    die "$label drift on $slice_name: expected '$expected', live '$actual'; refusing to start any child"
}

# Prove every contracted ceiling is live in cgroupfs. Anything missing,
# unreadable, or drifted fails closed here — before any child is spawned.
verify_slice_limits() {
  local dir period
  dir="$(slice_cgroup_dir)" ||
    die "cgroup directory for $slice_name not found after start; refusing to start any child"
  require_attr_value "$dir/memory.max" \
    "$(memory_to_bytes "$memory_max")" "MemoryMax=$(memory_to_bytes "$memory_max") bytes"
  require_attr_value "$dir/memory.swap.max" \
    "$(memory_to_bytes "$swap_max")" "MemorySwapMax=$(memory_to_bytes "$swap_max") bytes"
  require_attr_value "$dir/pids.max" "$tasks_max" "TasksMax=$tasks_max"
  period="$(awk '{print $2}' "$dir/cpu.max" 2>/dev/null)" ||
    die "cannot read $dir/cpu.max; CPUQuota not proven, refusing to start any child"
  require_attr_value "$dir/cpu.max" \
    "$(cpu_quota_to_cgroupfs "$cpu_quota" "$period")" "CPUQuota=$cpu_quota"
  return 0
}

report_plan() {
  printf 'dev-stack-scope: slice=%s MemoryMax=%s MemorySwapMax=%s TasksMax=%s CPUQuota=%s (aggregate over all operations)\n' \
    "$slice_name" "$memory_max" "$swap_max" "$tasks_max" "$cpu_quota"
}

# Establish the capped domain or abort with nothing spawned. A loaded slice
# is NEVER restarted — stopping it would SIGTERM every stack child inside;
# instead its live limits are re-proven against the contract.
ensure_slice() {
  verify_prerequisites
  if systemctl --user is-active --quiet "$slice_name" 2>/dev/null; then
    verify_slice_limits
    report_plan >&2
    return 0
  fi
  write_slice_unit
  systemctl --user daemon-reload
  # Starting the slice registers its ceilings in the user manager before
  # any stack process exists; refusal here leaves nothing behind except the
  # generated unit file, which the next successful start overwrites.
  systemctl --user start "$slice_name"
  verify_slice_limits
  report_plan >&2
}

run_scoped() {
  local mode="$1"
  shift
  local script="$here/$mode.sh"
  if inside_bounded_domain; then
    # Already inside the capped domain: exec directly. Nesting another
    # scope adds no isolation and only risks transient-unit name churn.
    exec bash "$script" "$@"
  fi
  # systemd-run registers the child scope unit (under the capped slice) in
  # the user manager BEFORE exec'ing the entry point, and fails without
  # starting anything if registration is refused. The per-operation name
  # keeps concurrent operations collision-free; all descendants inherit the
  # scope's cgroup, so builds, QEMU, cloud services, the OIDC issuer, and
  # emulators stay covered for the whole lifetime of the unit.
  exec systemd-run --user --quiet --scope \
    --unit="$scope_prefix-$mode-$$" \
    --collect \
    --same-dir \
    --property="Slice=$slice_name" \
    bash "$script" "$@"
}

main() {
  case "${1:-}" in
    up | check | provision | cloud | fabric | fabric-build)
      local mode="$1"
      shift
      local script="$here/$mode.sh"
      test -f "$script" || die "missing $script"
      ensure_slice # fail-closed gate: nothing below runs unless limits are proven
      run_scoped "$mode" "$@"
      ;;
    self-check)
      verify_prerequisites
      report_plan
      printf 'dev-stack-scope: prerequisites satisfied; %s deliberately NOT started (self-check is static)\n' "$slice_name"
      ;;
    *)
      die "usage: run-scoped.sh {up|check|provision|cloud|fabric|fabric-build|self-check} [args...]"
      ;;
  esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
