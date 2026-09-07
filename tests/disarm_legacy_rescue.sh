#!/usr/bin/env bash
# The disarm helper must remove generator/unit/wants/implant files and
# leave Nix-managed SSH/rescue paths untouched.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
script="${root}/ansible/files/voie-disarm-legacy-rescue.py"
python3 -m py_compile "$script"
fake="$(mktemp -d)"
trap 'rm -rf "$fake"' EXIT
mkdir -p \
  "$fake/etc/systemd/system-generators" \
  "$fake/etc/systemd/system/local-fs-pre.target.wants" \
  "$fake/etc/systemd/system/sysinit.target.wants" \
  "$fake/etc/systemd/system/sshd.service.d" \
  "$fake/etc/ssh" \
  "$fake/var/lib/voie-iso-rescue" \
  "$fake/var/lib/dropbear"
printf '#!/bin/sh\n' >"$fake/etc/systemd/system-generators/voie-iso-rescue"
chmod 755 "$fake/etc/systemd/system-generators/voie-iso-rescue"
printf '[Unit]\n' >"$fake/etc/systemd/system/voie-iso-rescue.service"
ln -s /etc/systemd/system/voie-iso-rescue.service \
  "$fake/etc/systemd/system/local-fs-pre.target.wants/voie-iso-rescue.service"
ln -s /etc/systemd/system/voie-iso-rescue.service \
  "$fake/etc/systemd/system/sysinit.target.wants/voie-iso-rescue.service"
cat >"$fake/etc/systemd/system/sshd.service.d/early.conf" <<'EOF'
[Unit]
DefaultDependencies=no
Before=local-fs.target shutdown.target
[Install]
WantedBy=local-fs-pre.target
EOF
printf 'implant\n' >"$fake/var/lib/voie-iso-rescue/run"
cat >"$fake/var/lib/voie-iso-rescue/net-up" <<'EOF'
#!/bin/sh
MAC='aa:bb:cc:dd:ee:ff'
CIDR='192.0.2.8/24'
GW='192.0.2.1'
EOF
chmod 755 "$fake/var/lib/voie-iso-rescue/net-up"
printf 'nix-ssh\n' >"$fake/etc/ssh/sshd_config"
printf 'managed-dropbear\n' >"$fake/var/lib/dropbear/dropbear_ed25519_host_key"
python3 "$script" "$fake"
test ! -e "$fake/etc/systemd/system-generators/voie-iso-rescue"
test ! -e "$fake/etc/systemd/system/voie-iso-rescue.service"
test ! -e "$fake/etc/systemd/system/local-fs-pre.target.wants/voie-iso-rescue.service"
test ! -e "$fake/etc/systemd/system/sysinit.target.wants/voie-iso-rescue.service"
test ! -e "$fake/var/lib/voie-iso-rescue"
test ! -e "$fake/etc/systemd/system/sshd.service.d/early.conf"
test -f "$fake/etc/ssh/sshd_config"
test -f "$fake/var/lib/dropbear/dropbear_ed25519_host_key"
python3 "$script" "$fake"
python3 - "$script" <<'PY'
import importlib.util
import sys
from pathlib import Path

spec = importlib.util.spec_from_file_location("disarm", Path(sys.argv[1]))
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
assert mod.systemd_unit_already_gone(0, "")
assert mod.systemd_unit_already_gone(5, "")
assert mod.systemd_unit_already_gone(
    1, "Failed to disable unit: Unit voie-iso-rescue.service does not exist"
)
assert not mod.systemd_unit_already_gone(1, "Access denied")
assert not mod.systemd_unit_already_gone(1, "Failed to connect to bus")
PY
printf 'disarm_legacy_rescue: ok\n'
