#!/usr/bin/env bash
# Source-only PID ownership guard for local development processes.
#
# A numeric PID is never enough to identify a process: after exit, Linux may
# reuse it for an unrelated process. Each launcher records the process start
# time, the exact NUL-separated /proc/$pid/cmdline bytes, and its cgroup path
# beside the PID file. A stop is allowed only while all four identity facts
# still match and the recorded scope is still required. Any mismatch removes
# the stale record and performs NO signal.

pid_guard_starttime() {
  local pid="$1" stat_line rest
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  stat_line="$(cat "/proc/$pid/stat" 2>/dev/null)" || return 1
  # /proc/PID/stat's comm field is parenthesized and may contain spaces. Drop
  # everything through its final ')' so field 1 below is stat field 3;
  # stat field 22 (starttime) is therefore word 20 in the remainder.
  rest="${stat_line##*) }"
  set -- $rest
  (( $# >= 20 )) || return 1
  printf '%s\n' "${20}"
}

pid_guard_cgroup() {
  local pid="$1"
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  sed -n 's/^0:://p' "/proc/$pid/cgroup" 2>/dev/null
}

# Return the scope identity represented by a cgroup path. Every managed
# process must be a descendant of <prefix>.slice (or the legacy fixed scope).
pid_guard_scope_kind() {
  local cgroup_path="$1" scope_prefix="${2:-${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}}"
  case "$cgroup_path" in
    */"$scope_prefix".slice/* | */"$scope_prefix".scope)
      printf '%s\n' "$scope_prefix"
      return 0
      ;;
  esac
  return 1
}

pid_guard_discard() {
  local pid_file="$1"
  [[ -n "$pid_file" ]] || return 0
  rm -f -- "$pid_file" \
    "$pid_file.starttime" "$pid_file.cmdline" "$pid_file.cgroup" "$pid_file.scope"
}

# Record the identity of PID $1 under PID file $2. The exact command line
# bytes are copied from /proc at launch and become the expected command line
# for every later validation. $3 is the named required scope prefix.
pid_guard_record() {
  local pid="$1" pid_file="$2"
  local scope_prefix="${3:-${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}}"
  local starttime starttime_again cgroup_path scope_kind tmp_base

  [[ "$pid" =~ ^[0-9]+$ && -n "$pid_file" ]] || return 1
  starttime="$(pid_guard_starttime "$pid")" || return 1
  cgroup_path="$(pid_guard_cgroup "$pid")" || return 1
  [[ -n "$cgroup_path" ]] || return 1
  scope_kind="$(pid_guard_scope_kind "$cgroup_path" "$scope_prefix")" || return 1
  tmp_base="$pid_file.tmp.$$"

  rm -f -- "$tmp_base".*
  if ! cat "/proc/$pid/cmdline" >"$tmp_base.cmdline" 2>/dev/null; then
    rm -f -- "$tmp_base".*
    return 1
  fi
  [[ -s "$tmp_base.cmdline" ]] || {
    rm -f -- "$tmp_base".*
    return 1
  }
  starttime_again="$(pid_guard_starttime "$pid")" || {
    rm -f -- "$tmp_base".*
    return 1
  }
  [[ "$starttime" == "$starttime_again" ]] || {
    rm -f -- "$tmp_base".*
    return 1
  }

  printf '%s\n' "$pid" >"$tmp_base.pid"
  printf '%s\n' "$starttime" >"$tmp_base.starttime"
  printf '%s\n' "$cgroup_path" >"$tmp_base.cgroup"
  printf '%s\n' "$scope_kind" >"$tmp_base.scope"
  chmod 600 "$tmp_base".*

  # Publish sidecars only after every source fact was captured. A caller that
  # sees a partial record therefore fails closed and merely discards it.
  mv -f -- "$tmp_base.pid" "$pid_file"
  mv -f -- "$tmp_base.starttime" "$pid_file.starttime"
  mv -f -- "$tmp_base.cmdline" "$pid_file.cmdline"
  mv -f -- "$tmp_base.cgroup" "$pid_file.cgroup"
  mv -f -- "$tmp_base.scope" "$pid_file.scope"
  chmod 600 "$pid_file" "$pid_file.starttime" "$pid_file.cmdline" \
    "$pid_file.cgroup" "$pid_file.scope"
}

# Validate without sending any signal. This checks PID, starttime, exact
# command line, exact cgroup path, and the required named scope.
pid_guard_validate() {
  local pid_file="$1" scope_prefix="${2:-${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}}"
  local pid recorded_start current_start recorded_cgroup current_cgroup recorded_scope
  [[ -s "$pid_file" && -s "$pid_file.starttime" && -s "$pid_file.cmdline" &&
    -s "$pid_file.cgroup" && -s "$pid_file.scope" ]] || return 1
  pid="$(cat "$pid_file" 2>/dev/null)" || return 1
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  recorded_start="$(cat "$pid_file.starttime" 2>/dev/null)" || return 1
  recorded_cgroup="$(cat "$pid_file.cgroup" 2>/dev/null)" || return 1
  recorded_scope="$(cat "$pid_file.scope" 2>/dev/null)" || return 1
  current_start="$(pid_guard_starttime "$pid")" || return 1
  [[ "$current_start" == "$recorded_start" ]] || return 1
  # /proc/PID/cmdline reports size 0, so cmp against the recorded file
  # treats it as empty and always mismatches. Read the bytes first.
  local live_cmdline="$pid_file.live.$$"
  if ! cat "/proc/$pid/cmdline" >"$live_cmdline" 2>/dev/null; then
    rm -f "$live_cmdline"
    return 1
  fi
  if ! cmp -s "$live_cmdline" "$pid_file.cmdline"; then
    rm -f "$live_cmdline"
    return 1
  fi
  rm -f "$live_cmdline"
  current_cgroup="$(pid_guard_cgroup "$pid")" || return 1
  [[ "$current_cgroup" == "$recorded_cgroup" ]] || return 1
  [[ "$recorded_scope" == "$scope_prefix" ]] || return 1
  [[ "$(pid_guard_scope_kind "$current_cgroup" "$scope_prefix")" == "$scope_prefix" ]] || return 1
}

# Validate immediately before TERM and again immediately before KILL. If the
# process exits, changes identity, or leaves its cgroup at any point, remove
# only the stale records and never signal the new occupant.
pid_guard_stop() {
  local pid_file="$1" scope_prefix="${2:-${VOIE_DEV_STACK_SCOPE:-voie-dev-stack}}"
  local pid
  if ! pid_guard_validate "$pid_file" "$scope_prefix"; then
    pid_guard_discard "$pid_file"
    return 0
  fi
  pid="$(cat "$pid_file")"
  if ! kill -TERM "$pid" 2>/dev/null; then
    pid_guard_discard "$pid_file"
    return 0
  fi
  for _ in $(seq 1 20); do
    pid_guard_validate "$pid_file" "$scope_prefix" || {
      pid_guard_discard "$pid_file"
      return 0
    }
    kill -0 "$pid" 2>/dev/null || {
      pid_guard_discard "$pid_file"
      return 0
    }
    sleep 0.5
  done
  if pid_guard_validate "$pid_file" "$scope_prefix"; then
    kill -KILL "$pid" 2>/dev/null || true
  fi
  pid_guard_discard "$pid_file"
}
