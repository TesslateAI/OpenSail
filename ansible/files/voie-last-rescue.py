"""Last ISO-rescue implant for the installed NixOS Fabric disk.

Run as root from Debian rescue. Does not reboot.

The live NixOS generation does not contain this branch's dropbear or
sshd-before-local-fs units. A leftover voie-ws mount hangs local-fs.target,
and sshd never starts. This implant writes a self-contained dropbear
(Debian binary + its libc) onto the NixOS root so the next NixOS boot:

- assigns the rescue NIC address by MAC (independent of NixOS networking)
- listens on TCP/22 and TCP/2222 BEFORE local-fs.target
- is pulled in by local-fs-pre.target AND kernel systemd.wants=
- masks leftover ws-root / workspaces mounts so local-fs can finish

Prints READY_TO_REBOOT only when every on-disk proof passes. The operator
must not reboot until that line is printed and the files are re-read from
the mounted root.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

IMPLANT_DIR = Path("/var/lib/voie-iso-rescue")
UNIT_NAME = "voie-iso-rescue.service"
KERNEL_WANTS = f"systemd.wants={UNIT_NAME}"
KERNEL_MASK = "systemd.mask=var-lib-voie-workspaces.mount"
FSTAB_MARKERS = (
    "ws-root",
    "voie-ws",
    "/var/lib/voie/workspaces",
    "voie--ws",
)
MASK_UNITS = (
    "var-lib-voie-workspaces.mount",
    "var-lib-voie.mount",
    "var-lib-voie-workspaces.automount",
)


def run(argv: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        check=kwargs.pop("check", True),
        text=True,
        capture_output=True,
        **kwargs,
    )


def log(msg: str) -> None:
    sys.stderr.write(msg + "\n")
    sys.stderr.flush()


def fail(msg: str, code: int = 1) -> None:
    log("FAIL: " + msg)
    raise SystemExit(code)


def is_root() -> bool:
    return os.geteuid() == 0


def on_debian_rescue() -> bool:
    return Path("/etc/debian_version").exists() and not Path("/etc/NIXOS").exists()


def on_nixos() -> bool:
    return Path("/etc/NIXOS").exists()


def rescue_net() -> tuple[str, str, str]:
    """Return (mac, cidr, gateway) for the default-route NIC."""
    route = run(["ip", "-4", "-o", "route", "show", "default"]).stdout.strip()
    if not route:
        fail("no IPv4 default route on rescue; cannot pin the implant address")
    parts = route.split()
    try:
        gw = parts[parts.index("via") + 1]
        nic = parts[parts.index("dev") + 1]
    except (ValueError, IndexError):
        fail("cannot parse default route: " + route)
    addr = run(["ip", "-4", "-o", "addr", "show", "dev", nic]).stdout.strip()
    cidr = ""
    for tok in addr.split():
        if "/" in tok and tok[0].isdigit():
            cidr = tok
            break
    if not cidr:
        fail("no IPv4 CIDR on rescue NIC " + nic)
    mac_path = Path("/sys/class/net") / nic / "address"
    if not mac_path.exists():
        fail("no MAC for rescue NIC " + nic)
    mac = mac_path.read_text(encoding="utf-8").strip().lower()
    log(f"rescue net nic={nic} mac={mac} cidr={cidr} gw={gw}")
    return mac, cidr, gw


def find_nixos_candidates() -> list[str]:
    """Prefer LABEL=nixos. Live estate: OS is nvme1n1p2, Fabric VG is nvme0n1."""
    candidates: list[str] = []
    by_label = Path("/dev/disk/by-label/nixos")
    if by_label.exists():
        candidates.append(str(by_label.resolve()))
    labeled: list[str] = []
    ext4: list[str] = []
    blkid = run(["blkid", "-o", "device"], check=False)
    for dev in blkid.stdout.splitlines():
        dev = dev.strip()
        if not dev or dev in candidates:
            continue
        probe = run(["blkid", "-o", "export", dev], check=False)
        env = dict(
            line.split("=", 1)
            for line in probe.stdout.splitlines()
            if "=" in line
        )
        if env.get("TYPE") == "LVM2_member":
            continue
        if env.get("LABEL") == "nixos":
            labeled.append(dev)
        elif env.get("TYPE") == "ext4":
            ext4.append(dev)
    for dev in labeled + ext4:
        if dev not in candidates:
            candidates.append(dev)
    if not candidates:
        fail("cannot find NixOS root (label nixos or ext4, not the Fabric PV)")
    return candidates


def mount_nixos(dest: Path) -> Path:
    dest.mkdir(parents=True, exist_ok=True)
    mounted = run(["findmnt", "-n", "-o", "SOURCE", str(dest)], check=False)
    if mounted.returncode == 0 and mounted.stdout.strip():
        log("already mounted " + mounted.stdout.strip() + " at " + str(dest))
        if not (dest / "etc/NIXOS").exists():
            fail(mounted.stdout.strip() + " is mounted but has no /etc/NIXOS")
        return dest
    last_err = "no candidates"
    for source in find_nixos_candidates():
        log("trying NixOS root " + source)
        rc = run(["mount", source, str(dest)], check=False)
        if rc.returncode != 0:
            last_err = (rc.stderr or rc.stdout or "mount failed").strip()
            continue
        if (dest / "etc/NIXOS").exists():
            log("mounted " + source + " at " + str(dest))
            return dest
        run(["umount", str(dest)], check=False)
        last_err = source + " mounted but has no /etc/NIXOS"
    fail("cannot mount NixOS root: " + last_err)


def mount_boot(root: Path) -> list[Path]:
    """Mount ESP/boot if needed; return dirs to scan for bootloader entries."""
    found: list[Path] = []
    for candidate in (root / "boot", root / "boot/efi", root / "efi"):
        if candidate.is_dir():
            found.append(candidate)
    src = run(["findmnt", "-n", "-o", "SOURCE", str(root)], check=False).stdout.strip()
    parent = ""
    if src:
        parent = run(["lsblk", "-no", "PKNAME", src], check=False).stdout.strip()
    if parent:
        for node in Path("/dev").glob(parent + "p1"):
            fstype = run(["blkid", "-s", "TYPE", "-o", "value", str(node)], check=False)
            if (fstype.stdout or "").strip() not in ("vfat", "fat", "ext4"):
                continue
            dest = Path("/mnt/voie-esp")
            dest.mkdir(parents=True, exist_ok=True)
            already = run(["findmnt", "-n", "-o", "TARGET", str(node)], check=False)
            if str(dest) in (already.stdout or ""):
                found.append(dest)
                continue
            rc = run(["mount", str(node), str(dest)], check=False)
            if rc.returncode == 0:
                found.append(dest)
                log("mounted boot " + str(node) + " at " + str(dest))
    return found


def copy_dynamic_binary(binary: Path, dest_dir: Path, name: str) -> Path:
    dest_dir.mkdir(parents=True, exist_ok=True)
    lib_dir = dest_dir / "lib"
    lib_dir.mkdir(parents=True, exist_ok=True)
    target = dest_dir / name
    shutil.copy2(binary, target)
    os.chmod(target, 0o755)
    ldd = run(["ldd", str(binary)])
    interpreter = None
    for line in ldd.stdout.splitlines():
        line = line.strip()
        if "linux-vdso" in line:
            continue
        if line.startswith("/") and "ld-linux" in line:
            interpreter = line.split()[0]
        elif "=>" in line:
            src = line.split("=>", 1)[1].strip().split()[0]
            if src.startswith("("):
                continue
            if Path(src).exists():
                shutil.copy2(src, lib_dir / Path(src).name)
        elif "ld-linux" in line:
            tok = line.split()[0]
            if Path(tok).exists():
                interpreter = tok
    if interpreter is None:
        # ldd prints "linux-vdso" and "/lib64/ld-linux-x86-64.so.2 (0x...)"
        for line in ldd.stdout.splitlines():
            for tok in line.split():
                if "ld-linux" in tok and Path(tok).exists():
                    interpreter = tok
    if not interpreter or not Path(interpreter).exists():
        fail("cannot find dynamic linker for " + str(binary) + "\n" + ldd.stdout)
    ld_dest = dest_dir / "ld-linux.so"
    shutil.copy2(interpreter, ld_dest)
    os.chmod(ld_dest, 0o755)
    return target


def ensure_dropbear_bin() -> Path:
    candidate = Path("/usr/sbin/dropbear")
    if candidate.exists():
        return candidate
    log("installing dropbear-bin on rescue")
    run(
        ["apt-get", "update"],
        check=False,
        env={**os.environ, "DEBIAN_FRONTEND": "noninteractive"},
    )
    apt = run(
        ["apt-get", "install", "-y", "dropbear-bin"],
        check=False,
        env={**os.environ, "DEBIAN_FRONTEND": "noninteractive"},
    )
    if apt.returncode != 0:
        log(apt.stdout + apt.stderr)
        fail("apt-get install dropbear-bin failed")
    if not candidate.exists():
        fail("dropbear-bin installed but /usr/sbin/dropbear missing")
    return candidate


def ensure_dropbearkey() -> Path:
    candidate = Path("/usr/bin/dropbearkey")
    if candidate.exists():
        return candidate
    alt = Path("/usr/sbin/dropbearkey")
    if alt.exists():
        return alt
    fail("dropbearkey missing after dropbear-bin install")


def extra_operator_key() -> str | None:
    """Optional extra key from argv or env. Never a baked-in break-glass key."""
    args = sys.argv[1:]
    path = None
    if "--operator-key-file" in args:
        index = args.index("--operator-key-file")
        if index + 1 >= len(args):
            fail("--operator-key-file requires a path")
        path = args[index + 1]
    elif os.environ.get("VOIE_RESCUE_OPERATOR_KEY_FILE"):
        path = os.environ["VOIE_RESCUE_OPERATOR_KEY_FILE"]
    if not path:
        return None
    text = Path(path).read_text(encoding="utf-8", errors="replace")
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("ssh-"):
            return line
    fail("operator key file contains no ssh public key")


def collect_keys(root: Path, extra: str | None = None) -> str:
    keys: list[str] = []
    if extra:
        keys.append(extra)
    for path in (
        root / "etc/ssh/authorized_keys.d/root",
        root / "root/.ssh/authorized_keys",
        root / "etc/dropbear/authorized_keys",
    ):
        real = resolve_on_root(root, path) if (path.exists() or path.is_symlink()) else path
        if not real.is_file():
            continue
        for line in real.read_text(encoding="utf-8", errors="replace").splitlines():
            line = line.strip()
            if line.startswith("ssh-") and line not in keys:
                keys.append(line)
    if not keys:
        fail("no operator SSH keys found on the target root; pass --operator-key-file")
    return "\n".join(keys) + "\n"


def resolve_on_root(root: Path, path: Path) -> Path:
    """Follow absolute symlinks as they will appear on the installed root.

    NixOS /etc/systemd/system -> /etc/static/systemd/system -> /nix/store/...
    From Debian rescue those targets do not exist at the rescue /, so each
    component has to be resolved against the mounted root, not the kernel.
    """
    seen: set[str] = set()
    current = Path(path)
    root = root.resolve()
    while True:
        if not str(current).startswith(str(root)):
            if str(current).startswith("/"):
                current = root / str(current).lstrip("/")
            else:
                return current
        try:
            rel = current.relative_to(root)
        except ValueError:
            return current
        probe = root
        hit: Path | None = None
        for part in rel.parts:
            probe = probe / part
            if probe.is_symlink():
                hit = probe
                break
        if hit is None:
            return current
        key = str(hit)
        if key in seen:
            return current
        seen.add(key)
        target = os.readlink(hit)
        rest_parts = rel.parts[len(hit.relative_to(root).parts) :]
        rest = Path(*rest_parts) if rest_parts else Path()
        if target.startswith("/"):
            base = root / target.lstrip("/")
        else:
            base = hit.parent / target
        current = base / rest if rest.parts else base


def farm_symlinks(root: Path, src: Path, dest: Path) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    for child in src.iterdir():
        out = dest / child.name
        if child.is_dir() and not child.is_symlink():
            farm_symlinks(root, child, out)
            continue
        if out.exists() or out.is_symlink():
            continue
        if child.is_symlink():
            out.symlink_to(os.readlink(child))
        else:
            rel = str(child)
            prefix = str(root)
            if rel.startswith(prefix):
                rel = rel[len(prefix) :]
            if not rel.startswith("/"):
                rel = "/" + rel
            out.symlink_to(rel)


def ensure_writable_dir(root: Path, rel: str) -> Path:
    """Replace a NixOS /etc/static symlink with a writable overlay of store units."""
    path = root / rel
    bak = path.with_name(path.name + ".voie-static")
    src = None
    if bak.is_symlink() or bak.exists():
        src = resolve_on_root(root, bak)
    if src is None and (path.is_symlink() or path.exists()):
        src = resolve_on_root(root, path)
    overlay_ready = path.is_dir() and not path.is_symlink()
    needs_farm = src is not None and src.is_dir() and src != path and (
        not overlay_ready or not (path / "sshd.service").exists()
    )
    if overlay_ready and not needs_farm:
        return path
    if path.is_symlink() or path.is_file():
        if not bak.exists():
            path.rename(bak)
            if src is None:
                src = resolve_on_root(root, bak)
        else:
            path.unlink()
    if path.is_dir() and not path.is_symlink() and needs_farm:
        # Incomplete overlay from an earlier run: keep extra files, farm store units.
        pass
    else:
        path.mkdir(parents=True, exist_ok=True)
    if src is not None and src.is_dir() and src != path:
        log("farming systemd units from " + str(src) + " -> " + str(path))
        farm_symlinks(root, src, path)
    return path


def write_wrapper(dest_dir: Path, mac: str, cidr: str, gw: str) -> None:
    net_up = dest_dir / "net-up"
    net_up.write_text(
        f"""#!/bin/sh
# Early NixOS PATH has no basename(1). Stay in shell builtins plus ip.
set -eu
PATH="/run/current-system/sw/bin:/run/current-system/sw/sbin:/nix/var/nix/profiles/system/sw/bin:/sbin:/bin:/usr/sbin:/usr/bin"
MAC='{mac}'
CIDR='{cidr}'
GW='{gw}'
ipbin=
for cand in /run/current-system/sw/bin/ip /nix/var/nix/profiles/system/sw/bin/ip /sbin/ip /usr/sbin/ip; do
  if [ -x "$cand" ]; then
    ipbin=$cand
    break
  fi
done
for nic in /sys/class/net/*; do
  name=${{nic##*/}}
  [ "$name" = lo ] && continue
  addr=$(cat "$nic/address" 2>/dev/null || true)
  [ "$addr" = "$MAC" ] || continue
  if [ -n "$ipbin" ]; then
    "$ipbin" link set "$name" up
    "$ipbin" addr add "$CIDR" dev "$name" 2>/dev/null || true
    "$ipbin" route add default via "$GW" dev "$name" 2>/dev/null || true
  fi
done
iptablesbin=
for cand in /run/current-system/sw/bin/iptables /nix/var/nix/profiles/system/sw/bin/iptables /sbin/iptables; do
  if [ -x "$cand" ]; then
    iptablesbin=$cand
    break
  fi
done
if [ -n "$iptablesbin" ]; then
  "$iptablesbin" -I INPUT -p tcp --dport 22 -j ACCEPT 2>/dev/null || true
  "$iptablesbin" -I INPUT -p tcp --dport 2222 -j ACCEPT 2>/dev/null || true
fi
exit 0
""",
        encoding="utf-8",
    )
    os.chmod(net_up, 0o755)

    run_sh = dest_dir / "run"
    run_sh.write_text(
        """#!/bin/sh
set -eu
DIR=/var/lib/voie-iso-rescue
exec "$DIR/ld-linux.so" --library-path "$DIR/lib" "$DIR/dropbear" "$@"
""",
        encoding="utf-8",
    )
    os.chmod(run_sh, 0o755)

    unit = """[Unit]
Description=ISO-rescue dropbear (before local-fs)
DefaultDependencies=no
After=systemd-udevd.service
Before=local-fs.target sshd.service shutdown.target
Conflicts=shutdown.target

[Service]
Type=simple
ExecStartPre=/var/lib/voie-iso-rescue/net-up
ExecStart=/var/lib/voie-iso-rescue/run -F -E -p 22 -p 2222 -s -g -r /var/lib/voie-iso-rescue/host_key
Restart=always
RestartSec=1s

[Install]
WantedBy=local-fs-pre.target
"""
    (dest_dir / "unit").write_text(unit, encoding="utf-8")


def install_unit(root: Path) -> None:
    system = ensure_writable_dir(root, "etc/systemd/system")
    unit_src = root / IMPLANT_DIR.relative_to("/") / "unit"
    unit_dst = system / UNIT_NAME
    shutil.copy2(unit_src, unit_dst)
    for wants_name in ("local-fs-pre.target.wants", "sysinit.target.wants"):
        wants = system / wants_name
        if wants.is_symlink():
            ensure_writable_dir(root, f"etc/systemd/system/{wants_name}")
            wants = system / wants_name
        wants.mkdir(parents=True, exist_ok=True)
        link = wants / UNIT_NAME
        if link.exists() or link.is_symlink():
            link.unlink()
        link.symlink_to(f"/etc/systemd/system/{UNIT_NAME}")

    gen_dir = ensure_writable_dir(root, "etc/systemd/system-generators")
    gen = gen_dir / "voie-iso-rescue"
    gen.write_text(
        f"""#!/bin/sh
# Recreate the wants symlink every boot so a later nixos-rebuild cannot
# silently drop the implant until flake dropbear is in the generation.
dest="${{1:-/run/systemd/generator}}"
mkdir -p "$dest/local-fs-pre.target.wants"
if [ -x /var/lib/voie-iso-rescue/run ] && [ -f /etc/systemd/system/{UNIT_NAME} ]; then
  ln -sf /etc/systemd/system/{UNIT_NAME} "$dest/local-fs-pre.target.wants/{UNIT_NAME}"
fi
""",
        encoding="utf-8",
    )
    os.chmod(gen, 0o755)


def install_sshd_dropin(root: Path) -> None:
    system = ensure_writable_dir(root, "etc/systemd/system")
    dropin_rel = "etc/systemd/system/sshd.service.d"
    dropin_dir = system / "sshd.service.d"
    if dropin_dir.is_symlink() or not dropin_dir.exists():
        dropin_dir = ensure_writable_dir(root, dropin_rel)
    dropin_dir.mkdir(parents=True, exist_ok=True)
    (dropin_dir / "early.conf").write_text(
        """[Unit]
DefaultDependencies=no
After=systemd-udevd.service
Before=local-fs.target shutdown.target
Conflicts=shutdown.target

[Install]
WantedBy=local-fs-pre.target
""",
        encoding="utf-8",
    )
    sshd = system / "sshd.service"
    wants = system / "local-fs-pre.target.wants"
    if wants.is_symlink():
        ensure_writable_dir(root, "etc/systemd/system/local-fs-pre.target.wants")
        wants = system / "local-fs-pre.target.wants"
    wants.mkdir(parents=True, exist_ok=True)
    link = wants / "sshd.service"
    if link.exists() or link.is_symlink():
        link.unlink()
    if sshd.exists() or sshd.is_symlink():
        link.symlink_to("/etc/systemd/system/sshd.service")


def mask_leftover_mounts(root: Path) -> None:
    systemd = ensure_writable_dir(root, "etc/systemd/system")
    extra: list[str] = list(MASK_UNITS)
    for path in systemd.rglob("*"):
        name = path.name
        if name.endswith(".mount") or name.endswith(".automount"):
            text = ""
            if path.is_file() and not path.is_symlink():
                try:
                    text = path.read_text(encoding="utf-8", errors="replace")
                except OSError:
                    text = ""
            if any(m in name or m in text for m in FSTAB_MARKERS):
                extra.append(name)
    fstab = root / "etc/fstab"
    fstab_real = resolve_on_root(root, fstab) if (fstab.exists() or fstab.is_symlink()) else fstab
    writable_fstab = fstab if (fstab.is_file() and not fstab.is_symlink()) else None
    if writable_fstab is None and fstab_real.is_file() and os.access(fstab_real, os.W_OK):
        writable_fstab = fstab_real
    if writable_fstab is not None and "/nix/store/" not in str(writable_fstab):
        text = writable_fstab.read_text(encoding="utf-8", errors="replace")
        bak = root / "etc/fstab.voie-iso-rescue.bak"
        if not bak.exists():
            shutil.copy2(writable_fstab, bak)
        new_lines: list[str] = []
        changed = False
        for line in text.splitlines(keepends=True):
            raw = line.lstrip()
            if raw.startswith("#") or not raw.strip():
                new_lines.append(line)
                continue
            if any(m in line for m in FSTAB_MARKERS):
                new_lines.append("# voie-iso-rescue: " + line)
                changed = True
            else:
                new_lines.append(line)
        if changed:
            try:
                writable_fstab.write_text("".join(new_lines), encoding="utf-8")
                log("commented leftover voie mounts in /etc/fstab")
            except OSError as exc:
                log("WARN: could not rewrite fstab: " + str(exc))
    for name in sorted(set(extra)):
        link = systemd / name
        if link.is_symlink() and os.readlink(link) == "/dev/null":
            continue
        if link.exists() or link.is_symlink():
            if not (name.endswith(".mount") or name.endswith(".automount")):
                continue
            link.unlink()
        link.symlink_to("/dev/null")
        log("masked " + name)


def patch_bootloader(boot_dirs: list[Path]) -> None:
    patched = 0
    extra = KERNEL_WANTS + " " + KERNEL_MASK
    for boot_dir in boot_dirs:
        for path in list(boot_dir.rglob("*.conf")) + list(boot_dir.rglob("grub.cfg")):
            if not path.is_file():
                continue
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            if "init=" not in text and "linux " not in text and "options " not in text:
                continue
            orig = text
            if KERNEL_WANTS not in text:
                # systemd-boot `options` line, or grub `linux` line.
                lines: list[str] = []
                for line in text.splitlines(keepends=True):
                    stripped = line.rstrip("\n")
                    is_linux = stripped.lstrip().startswith("linux ")
                    if (stripped.startswith("options ") or is_linux) and KERNEL_WANTS not in line:
                        nl = "\n" if line.endswith("\n") else ""
                        lines.append(stripped + " " + extra + nl)
                        continue
                    lines.append(line)
                text = "".join(lines)
            if text != orig:
                bak = path.with_suffix(path.suffix + ".voie-iso-rescue.bak")
                if not bak.exists():
                    shutil.copy2(path, bak)
                path.write_text(text, encoding="utf-8")
                patched += 1
                log("patched bootloader " + str(path))
    if patched == 0:
        log("WARN: no bootloader entries patched; kernel systemd.wants= not applied")


def write_keys(root: Path, dest_dir: Path, keys: str) -> None:
    for path in (
        dest_dir / "authorized_keys",
        root / "root/.ssh/authorized_keys",
        root / "etc/dropbear/authorized_keys",
    ):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(keys, encoding="utf-8")
        os.chmod(path, 0o600)
    # dropbear uses ~/.ssh/authorized_keys for root when started as root.


def generate_host_key(dest_dir: Path) -> None:
    host_key = dest_dir / "host_key"
    if host_key.exists():
        return
    keygen = ensure_dropbearkey()
    run([str(keygen), "-t", "ed25519", "-f", str(host_key)])
    os.chmod(host_key, 0o600)


def prove_dropbear(dest_dir: Path) -> None:
    ld = dest_dir / "ld-linux.so"
    dropbear = dest_dir / "dropbear"
    help_out = run(
        [
            str(ld),
            "--library-path",
            str(dest_dir / "lib"),
            str(dropbear),
            "-h",
        ],
        check=False,
    )
    # dropbear -h exits 1 but prints usage on stderr.
    blob = (help_out.stdout or "") + (help_out.stderr or "")
    if "Dropbear" not in blob and "dropbear" not in blob.lower():
        fail("copied dropbear does not run: " + blob)
    log("dropbear binary runs via copied ld-linux")


def prove_disk(root: Path, dest_dir: Path) -> None:
    proofs: list[tuple[str, bool]] = []

    def need(label: str, ok: bool) -> None:
        proofs.append((label, ok))
        log(("OK  " if ok else "BAD ") + label)

    need("implant dropbear", (dest_dir / "dropbear").is_file())
    need("implant ld-linux", (dest_dir / "ld-linux.so").is_file())
    need("implant run wrapper", (dest_dir / "run").is_file())
    need("implant net-up", (dest_dir / "net-up").is_file())
    need("implant host_key", (dest_dir / "host_key").is_file())
    authorized = (dest_dir / "authorized_keys").read_text(encoding="utf-8")
    need(
        "operator key in authorized_keys",
        any(line.strip().startswith("ssh-") for line in authorized.splitlines()),
    )
    unit = root / "etc/systemd/system" / UNIT_NAME
    need("unit file", unit.is_file())
    want = root / "etc/systemd/system/local-fs-pre.target.wants" / UNIT_NAME
    need("local-fs-pre wants symlink", want.is_symlink())
    need(
        "sshd early drop-in",
        (root / "etc/systemd/system/sshd.service.d/early.conf").is_file(),
    )
    need(
        "workspaces mount masked",
        (root / "etc/systemd/system/var-lib-voie-workspaces.mount").is_symlink()
        and os.readlink(root / "etc/systemd/system/var-lib-voie-workspaces.mount")
        == "/dev/null",
    )
    need("etc/NIXOS present", (root / "etc/NIXOS").exists())
    failed = [label for label, ok in proofs if not ok]
    if failed:
        fail("proofs failed: " + ", ".join(failed))


def vg_snapshot() -> None:
    out = run(["vgs", "--noheadings", "-o", "vg_name,vg_size,vg_free"], check=False)
    lvs = run(
        ["lvs", "--noheadings", "-o", "lv_name,vg_name,lv_size,lv_attr"],
        check=False,
    )
    log("vgs:\n" + (out.stdout or out.stderr or "(none)"))
    log("lvs:\n" + (lvs.stdout or lvs.stderr or "(none)"))


def self_test() -> None:
    import tempfile

    tmp = Path(tempfile.mkdtemp(prefix="voie-iso-rescue-"))
    root = tmp / "nixos"
    dest_dir = root / IMPLANT_DIR.relative_to("/")
    dest_dir.mkdir(parents=True)
    (root / "etc/NIXOS").parent.mkdir(parents=True)
    (root / "etc/NIXOS").write_text("", encoding="utf-8")
    (root / "etc/fstab").parent.mkdir(parents=True, exist_ok=True)
    (root / "etc/fstab").write_text(
        "UUID=abc / ext4 defaults 0 1\n"
        "/dev/voie-ws/ws-root /var/lib/voie/workspaces ext4 defaults 0 2\n",
        encoding="utf-8",
    )
    (root / "etc/systemd/system").mkdir(parents=True)
    (root / "root/.ssh").mkdir(parents=True)
    test_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA rescue-test"
    (root / "root/.ssh/authorized_keys").write_text(test_key + "\n", encoding="utf-8")
    (dest_dir / "dropbear").write_text("", encoding="utf-8")
    (dest_dir / "ld-linux.so").write_text("", encoding="utf-8")
    write_wrapper(dest_dir, "aa:bb:cc:dd:ee:ff", "10.0.0.2/24", "10.0.0.1")
    (dest_dir / "host_key").write_text("key", encoding="utf-8")
    write_keys(root, dest_dir, collect_keys(root))
    install_unit(root)
    install_sshd_dropin(root)
    mask_leftover_mounts(root)
    prove_disk(root, dest_dir)
    fstab = (root / "etc/fstab").read_text(encoding="utf-8")
    if "/dev/voie-ws/ws-root" in fstab and not any(
        line.lstrip().startswith("#") and "ws-root" in line
        for line in fstab.splitlines()
    ):
        fail("fstab ws-root was not commented")
    boot = tmp / "boot/loader/entries"
    boot.mkdir(parents=True)
    entry = boot / "nixos.conf"
    entry.write_text(
        "title NixOS\nlinux /EFI/nixos/bzImage\noptions init=/nix/store/x/init\n",
        encoding="utf-8",
    )
    patch_bootloader([tmp / "boot"])
    text = entry.read_text(encoding="utf-8")
    if KERNEL_WANTS not in text or KERNEL_MASK not in text:
        fail("bootloader options were not patched")
    log("SELF_TEST_OK " + str(tmp))


def main() -> None:
    if "--self-test" in sys.argv:
        self_test()
        return
    if not is_root():
        fail("must run as root")
    if "--reboot" in sys.argv:
        fail("refusing --reboot; reboot only after READY_TO_REBOOT and a second read-back")

    if on_debian_rescue():
        root = mount_nixos(Path("/mnt/voie-nixos"))
        boot_dirs = mount_boot(root)
    elif on_nixos():
        root = Path("/")
        boot_dirs = [Path("/boot"), Path("/boot/efi"), Path("/efi")]
        boot_dirs = [p for p in boot_dirs if p.is_dir()]
        log("running on NixOS; implanting onto /")
    else:
        fail("not Debian rescue and not NixOS; refuse to guess the root disk")

    mac, cidr, gw = rescue_net()
    dest_dir = root / IMPLANT_DIR.relative_to("/")
    dest_dir.mkdir(parents=True, exist_ok=True)

    binary = ensure_dropbear_bin()
    copy_dynamic_binary(binary, dest_dir, "dropbear")
    write_wrapper(dest_dir, mac, cidr, gw)
    generate_host_key(dest_dir)
    keys = collect_keys(root, extra_operator_key())
    write_keys(root, dest_dir, keys)
    prove_dropbear(dest_dir)
    install_unit(root)
    install_sshd_dropin(root)
    mask_leftover_mounts(root)
    patch_bootloader(boot_dirs)
    prove_disk(root, dest_dir)
    vg_snapshot()

    # Record the implant so the next SSH session can confirm without remounting.
    proof = dest_dir / "PROOF"
    proof.write_text(
        "READY_TO_REBOOT\n"
        f"mac={mac}\n"
        f"cidr={cidr}\n"
        f"gw={gw}\n"
        f"unit={UNIT_NAME}\n"
        f"ports=22,2222\n",
        encoding="utf-8",
    )
    os.chmod(proof, 0o644)
    log("")
    log("READY_TO_REBOOT")
    log("Do not reboot until this script has been run and READY_TO_REBOOT printed.")
    log("After NixOS boots, SSH to port 22 or 2222 with the operator key.")
    log("Then nixos-rebuild so flake dropbear + initrd SSH replace this implant.")


if __name__ == "__main__":
    main()
