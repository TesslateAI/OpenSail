"""Remove the transitional ISO-rescue implant after managed NixOS rescue is live.

Does not invent a second firewall or network owner. Uses the NixOS
`firewall` unit and scripted networking already on the host. Idempotent.
Optional argv[1] is a fake root for tests.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

UNIT_NAME = "voie-iso-rescue.service"
IMPLANT_DIR = Path("/var/lib/voie-iso-rescue")
GENERATOR_REL = Path("etc/systemd/system-generators/voie-iso-rescue")
UNIT_REL = Path("etc/systemd/system") / UNIT_NAME
WANTS = (
    Path("etc/systemd/system/local-fs-pre.target.wants") / UNIT_NAME,
    Path("etc/systemd/system/sysinit.target.wants") / UNIT_NAME,
)
RUN_WANTS = Path("run/systemd/generator/local-fs-pre.target.wants") / UNIT_NAME
EARLY_SSHD = Path("etc/systemd/system/sshd.service.d/early.conf")
NET_UP_REL = Path("var/lib/voie-iso-rescue/net-up")


def log(msg: str) -> None:
    sys.stderr.write(msg + "\n")
    sys.stderr.flush()


def unlink(path: Path) -> bool:
    if not (path.exists() or path.is_symlink()):
        return False
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink()
    return True


def live_root(root: Path) -> bool:
    return root == Path("/")


def run(argv: list[str], *, check: bool = True, allow_rc: tuple[int, ...] = ()) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(argv, check=False, capture_output=True, text=True)
    if result.returncode == 0 or result.returncode in allow_rc:
        return result
    if check:
        err = (result.stderr or result.stdout or "").strip()
        log(f"FAIL: {' '.join(argv)} rc={result.returncode}" + (f": {err}" if err else ""))
        raise SystemExit(1)
    return result


def systemd_unit_already_gone(returncode: int, text: str) -> bool:
    if returncode in (0, 5):
        return True
    err = (text or "").lower()
    return returncode == 1 and (
        "does not exist" in err or "not found" in err or "not loaded" in err
    )


def unit_is_active(name: str) -> bool:
    result = subprocess.run(
        ["systemctl", "is-active", "--quiet", name],
        check=False,
        capture_output=True,
        text=True,
    )
    return result.returncode == 0


def prove_managed_paths(when: str) -> None:
    if not unit_is_active("sshd.service") and not unit_is_active("sshd"):
        log(f"FAIL: managed operator SSH is not active {when}")
        raise SystemExit(1)
    if not unit_is_active("dropbear-rescue.service"):
        log(f"FAIL: managed dropbear-rescue is not active {when}")
        raise SystemExit(1)


def parse_net_up(path: Path) -> dict[str, str]:
    info = {"mac": "", "cidr": "", "gw": ""}
    if not path.is_file():
        return info
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        log(f"FAIL: cannot read implant net-up: {error}")
        raise SystemExit(1) from error
    for line in text.splitlines():
        match = re.match(r"^(MAC|CIDR|GW)='([^']*)'$", line.strip())
        if not match:
            continue
        key = {"MAC": "mac", "CIDR": "cidr", "GW": "gw"}[match.group(1)]
        info[key] = match.group(2)
    return info


def iface_for_mac(mac: str) -> str:
    if not mac:
        return ""
    sysfs = Path("/sys/class/net")
    if not sysfs.is_dir():
        return ""
    for nic in sysfs.iterdir():
        if nic.name == "lo":
            continue
        address = nic / "address"
        try:
            if address.read_text(encoding="utf-8").strip() == mac:
                return nic.name
        except OSError:
            continue
    return ""


def restart_unit_if_present(name: str) -> None:
    listed = subprocess.run(
        ["systemctl", "cat", name],
        check=False,
        capture_output=True,
        text=True,
    )
    if listed.returncode != 0:
        return
    run(["systemctl", "restart", name], check=True)


def remove_input_accept(port: str) -> None:
    argv = ["iptables", "-D", "INPUT", "-p", "tcp", "--dport", port, "-j", "ACCEPT"]
    while True:
        result = subprocess.run(argv, check=False, capture_output=True, text=True)
        if result.returncode != 0:
            return


def input_has_direct_accept(port: str) -> bool:
    listed = subprocess.run(
        ["iptables", "-S", "INPUT"],
        check=False,
        capture_output=True,
        text=True,
    )
    if listed.returncode != 0:
        return False
    needle = f"--dport {port} -j ACCEPT"
    for line in listed.stdout.splitlines():
        if line.startswith("-A INPUT") and needle in line:
            return True
    return False


def remove_legacy_net(info: dict[str, str]) -> None:
    iface = iface_for_mac(info["mac"])
    if iface and info["cidr"]:
        run(["ip", "addr", "del", info["cidr"], "dev", iface], check=False)
    if iface and info["gw"]:
        run(
            ["ip", "route", "del", "default", "via", info["gw"], "dev", iface],
            check=False,
        )
    run(["systemctl", "restart", "firewall"], check=True)
    restart_unit_if_present("network-setup")
    if iface:
        restart_unit_if_present(f"network-addresses-{iface}")
    restart_unit_if_present("dhcpcd")


def disarm_files(root: Path) -> None:
    unlink(root / GENERATOR_REL)
    unlink(root / UNIT_REL)
    for rel in WANTS:
        unlink(root / rel)
    unlink(root / RUN_WANTS)
    early = root / EARLY_SSHD
    if early.is_file() and not early.is_symlink():
        try:
            text = early.read_text(encoding="utf-8", errors="replace")
        except OSError:
            text = ""
        if "Before=local-fs.target" in text and "WantedBy=local-fs-pre.target" in text:
            unlink(early)
            dropin = early.parent
            try:
                if dropin.is_dir() and not any(dropin.iterdir()):
                    dropin.rmdir()
            except OSError:
                pass
    implant = root / IMPLANT_DIR.relative_to("/")
    if implant.exists() or implant.is_symlink():
        if implant.is_dir() and not implant.is_symlink():
            shutil.rmtree(implant)
        else:
            unlink(implant)


def disarm_live_runtime(info: dict[str, str]) -> None:
    prove_managed_paths("before legacy implant cleanup")
    disable = subprocess.run(
        ["systemctl", "disable", "--now", UNIT_NAME],
        check=False,
        capture_output=True,
        text=True,
    )
    # 0 = stopped, 5 = not loaded. NixOS reports rc=1 "does not exist"
    # when the unit file is already gone; that is the same already-absent
    # case and must not fail the helper.
    if not systemd_unit_already_gone(
        disable.returncode, disable.stderr or disable.stdout or ""
    ):
        err = (disable.stderr or disable.stdout or "").strip()
        log(f"FAIL: systemctl disable --now {UNIT_NAME} rc={disable.returncode}: {err}")
        raise SystemExit(1)
    for port in ("22", "2222"):
        remove_input_accept(port)
    remove_legacy_net(info)
    run(["systemctl", "daemon-reload"], check=True)
    prove_managed_paths("after legacy implant cleanup")
    for port in ("22", "2222"):
        if input_has_direct_accept(port):
            log(f"FAIL: legacy INPUT ACCEPT {port} still present")
            raise SystemExit(1)


def remaining_paths(root: Path) -> list[Path]:
    return [
        root / GENERATOR_REL,
        root / UNIT_REL,
        root / IMPLANT_DIR.relative_to("/"),
        root / WANTS[0],
        root / WANTS[1],
        root / RUN_WANTS,
        root / NET_UP_REL,
    ]


def main() -> int:
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/")
    if live_root(root) and os.geteuid() != 0:
        log("FAIL: live disarm requires root")
        return 1
    net_up = root / NET_UP_REL
    info = parse_net_up(net_up)
    if live_root(root):
        disarm_live_runtime(info)
    disarm_files(root)
    leftover = [path for path in remaining_paths(root) if path.exists() or path.is_symlink()]
    if leftover:
        log("FAIL: legacy ISO-rescue implant still present")
        return 1
    log("legacy ISO-rescue implant disarmed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
