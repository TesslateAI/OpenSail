"""Signal processes holding voie-ws mapper devices.

Used by the DESTROY cutover. Walks /proc so the wipe does not depend on
fuser/psmisc, which the live Fabric image may not have until nixos-rebuild.
Never kills pid 1, this process, or sshd.
"""

from __future__ import annotations

import os
import signal
import sys
import time

MARKERS = ("voie--ws", "/dev/mapper/voie", "voie-ws/")
SKIP_COMM = {
    "sshd",
    "sshd-session",
    "dropbear",
    "systemd",
    "init",
}


def comm(pid: int) -> str:
    try:
        with open(f"/proc/{pid}/comm", encoding="utf-8", errors="replace") as handle:
            return handle.read().strip()
    except OSError:
        return ""


def holds_voie_ws(pid: int) -> bool:
    paths = [f"/proc/{pid}/cwd", f"/proc/{pid}/exe"]
    fd_dir = f"/proc/{pid}/fd"
    try:
        for name in os.listdir(fd_dir):
            paths.append(f"{fd_dir}/{name}")
    except OSError:
        pass
    for path in paths:
        try:
            target = os.readlink(path)
        except OSError:
            continue
        if any(marker in target for marker in MARKERS):
            return True
    try:
        with open(f"/proc/{pid}/maps", encoding="utf-8", errors="replace") as handle:
            text = handle.read()
        return any(marker in text for marker in MARKERS)
    except OSError:
        return False


def holders() -> list[int]:
    skip = {1, os.getpid(), os.getppid()}
    found: list[int] = []
    try:
        names = os.listdir("/proc")
    except OSError:
        return found
    for name in names:
        if not name.isdigit():
            continue
        pid = int(name)
        if pid in skip:
            continue
        if comm(pid) in SKIP_COMM:
            continue
        if holds_voie_ws(pid):
            found.append(pid)
    return found


def shoot(sig: int) -> int:
    sent = 0
    for pid in holders():
        try:
            os.kill(pid, sig)
            sent += 1
        except OSError:
            pass
    return sent


def list_lvs() -> int:
    """Print LV names with thin pools last so linear volumes drop first."""
    import subprocess

    try:
        out = subprocess.check_output(
            [
                "lvs",
                "--noheadings",
                "--separator",
                "|",
                "-o",
                "lv_name,lv_layout",
                "voie-ws",
            ],
            text=True,
            timeout=25,
        )
    except (FileNotFoundError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return 1
    linear: list[str] = []
    pools: list[str] = []
    for line in out.splitlines():
        parts = [part.strip() for part in line.split("|")]
        if len(parts) < 2 or not parts[0]:
            continue
        name, layout = parts[0], parts[1]
        if "pool" in layout:
            pools.append(name)
        else:
            linear.append(name)
    for name in linear + pools:
        print(name)
    return 0


def main() -> int:
    which = sys.argv[1] if len(sys.argv) > 1 else "BOTH"
    if which == "LIST":
        return list_lvs()
    if which == "TERM":
        print(f"term {shoot(signal.SIGTERM)}")
        return 0
    if which == "KILL":
        print(f"kill {shoot(signal.SIGKILL)}")
        return 0
    print(f"term {shoot(signal.SIGTERM)}")
    time.sleep(2)
    print(f"kill {shoot(signal.SIGKILL)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
