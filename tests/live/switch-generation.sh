#!/usr/bin/env bash
# Install one NixOS system closure on an already-enrolled host: set the
# system profile, run switch-to-configuration switch, prove
# /run/current-system and ExecStart match that closure, and remove leftover
# legacy /run overrides. Reboot is a separate operator/C8 step.
set -euo pipefail

if [[ $# -ne 3 ]]; then
  printf 'usage: switch-generation.sh <ssh-host> <flake-attr> <unit>\n' >&2
  exit 2
fi

host="$1"
flake_attr="$2"
unit="$3"

if [[ "$flake_attr" != .* ]]; then
  printf 'switch-generation: flake attr must start with .\n' >&2
  exit 2
fi

printf 'switch-generation: building %s\n' "$flake_attr" >&2
toplevel="$(nix build --no-link --print-out-paths "$flake_attr")"
printf 'switch-generation: toplevel %s\n' "$toplevel" >&2

expected_exec="$(grep -E '^ExecStart=' "$toplevel/etc/systemd/system/${unit}.service" | tail -n1 | sed 's/^ExecStart=//')"
if [[ -z "$expected_exec" ]]; then
  printf 'switch-generation: %s has no ExecStart\n' "$unit" >&2
  exit 1
fi

export NIX_SSHOPTS="-o BatchMode=yes -o ConnectTimeout=20"
copy_path="/usr/bin:/bin:${HOME}/.nix-profile/bin"
printf 'switch-generation: copying closure to %s\n' "$host" >&2
all_file="$(mktemp)"
missing_file="$(mktemp)"
nix-store -qR "$toplevel" >"$all_file"
ssh -o BatchMode=yes "$host" 'while IFS= read -r p; do [ -e "$p" ] || printf "%s\n" "$p"; done' <"$all_file" >"$missing_file" || true
if [[ ! -s "$missing_file" ]]; then
  printf 'switch-generation: all store paths already present on %s\n' "$host" >&2
else
  printf 'switch-generation: importing %s missing paths via nix-store export\n' "$(wc -l <"$missing_file")" >&2
  # Concatenated NAR stream. Batch to stay under ARG_MAX. Do not use
  # `nix copy` over nix-store --serve: that path is ~150KB/s here.
  if ! PATH="$copy_path" xargs -a "$missing_file" -n 32 nix-store --export \
    | ssh -o BatchMode=yes "$host" 'if sudo -n true >/dev/null 2>&1; then sudo -n nix-store --import; else nix-store --import; fi'; then
    printf 'switch-generation: export/import failed\n' >&2
    rm -f "$all_file" "$missing_file"
    exit 1
  fi
fi
rm -f "$all_file" "$missing_file"

printf 'switch-generation: switching %s\n' "$host" >&2
# nixos-rebuild switch sets the profile, then activates. Running only
# switch-to-configuration leaves /nix/var/nix/profiles/system on the old
# generation so GC can delete the repaired closure before reboot.
set_profile="nix-env -p /nix/var/nix/profiles/system --set ${toplevel}"
if ssh -o BatchMode=yes "$host" "sudo -n ${set_profile}"; then
  :
else
  ssh -o BatchMode=yes "$host" "$set_profile"
fi
# Fabric ssh wrapper already sudoes; control needs sudo -n. Try sudo first.
if ssh -o BatchMode=yes "$host" "sudo -n ${toplevel}/bin/switch-to-configuration switch"; then
  :
else
  ssh -o BatchMode=yes "$host" "${toplevel}/bin/switch-to-configuration switch"
fi

# Remove leftover legacy /run overrides from older estates. Persistent
# deployment is the NixOS system generation.
cleanup="rm -rf /run/systemd/system/${unit}.service.d /run/systemd/system/voie-activation-broker@.service.d; systemctl daemon-reload; systemctl restart ${unit}"
if ssh -o BatchMode=yes "$host" "sudo -n bash -lc $(printf '%q' "$cleanup")"; then
  :
else
  ssh -o BatchMode=yes "$host" "bash -lc $(printf '%q' "$cleanup")"
fi

live_exec="$(ssh -o BatchMode=yes "$host" "systemctl show ${unit} -p ExecStart --value" || true)"
if [[ "$live_exec" != *"$expected_exec"* ]]; then
  printf 'switch-generation: ExecStart mismatch\n expected substring: %s\n live: %s\n' "$expected_exec" "$live_exec" >&2
  exit 1
fi

drops="$(ssh -o BatchMode=yes "$host" "systemctl show ${unit} -p DropInPaths --value")"
if echo "$drops" | grep -q '/run/systemd/system'; then
  printf 'switch-generation: legacy /run override still attached: %s\n' "$drops" >&2
  exit 1
fi
live_id="$(ssh -o BatchMode=yes "$host" "readlink -f /run/current-system")"
if [[ "$live_id" != "$toplevel" ]]; then
  printf 'switch-generation: current-system %s is not %s\n' "$live_id" "$toplevel" >&2
  exit 1
fi
profile_id="$(ssh -o BatchMode=yes "$host" "readlink -f /nix/var/nix/profiles/system")"
if [[ "$profile_id" != "$toplevel" ]]; then
  printf 'switch-generation: system profile %s is not %s\n' "$profile_id" "$toplevel" >&2
  exit 1
fi

printf 'switch-generation: %s now runs %s from %s\n' "$unit" "$expected_exec" "$toplevel" >&2
echo "$toplevel"
