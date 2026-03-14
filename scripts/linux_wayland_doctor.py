#!/usr/bin/env python3
"""Assess whether the Linux GTK frontend can run on a Wayland session."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Callable, Dict, Optional


ROOT = Path(__file__).resolve().parent.parent
PACMAN_INSTALL = "sudo pacman -S --needed gtk4 libadwaita vte4 pkgconf rustup clang"
RUN_COMMAND = "GDK_BACKEND=wayland cargo run --manifest-path linux/Cargo.toml -p cmux-linux"


def _command_exists(name: str) -> bool:
    return shutil.which(name) is not None


def _pkg_config_modversion(name: str) -> Optional[str]:
    try:
        output = subprocess.check_output(
            ["pkg-config", "--modversion", name],
            cwd=ROOT,
            stderr=subprocess.DEVNULL,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    version = output.strip()
    return version or None


def assess_environment(
    env: Optional[Dict[str, str]] = None,
    command_exists: Callable[[str], bool] = _command_exists,
    pkg_config_modversion: Callable[[str], Optional[str]] = _pkg_config_modversion,
) -> Dict[str, object]:
    env = dict(os.environ if env is None else env)

    wayland_display = env.get("WAYLAND_DISPLAY", "").strip()
    session_type = env.get("XDG_SESSION_TYPE", "").strip().lower()
    wayland_ok = bool(wayland_display) and session_type == "wayland"

    commands = {
        "cargo": command_exists("cargo"),
        "pkg-config": command_exists("pkg-config"),
    }
    packages = {
        "gtk4": pkg_config_modversion("gtk4"),
        "libadwaita-1": pkg_config_modversion("libadwaita-1"),
        "vte-2.91-gtk4": pkg_config_modversion("vte-2.91-gtk4"),
    }

    missing = []
    if not wayland_ok:
        missing.append("wayland-session")
    if not commands["cargo"]:
        missing.append("cargo")
    if not commands["pkg-config"]:
        missing.append("pkg-config")
    for name, version in packages.items():
        if not version:
            missing.append(name)

    ready = not missing
    return {
        "ready": ready,
        "missing": missing,
        "install_command": PACMAN_INSTALL,
        "run_command": RUN_COMMAND,
        "repo_root": str(ROOT),
        "terminal_backend": {
            "active": "vte",
            "requested": None,
            "supported": ["vte"],
            "note": None,
        },
        "wayland": {
            "ok": wayland_ok,
            "WAYLAND_DISPLAY": wayland_display,
            "XDG_SESSION_TYPE": session_type,
        },
        "commands": commands,
        "packages": packages,
    }


def _print_human(report: Dict[str, object]) -> None:
    print(f"repo_root: {report['repo_root']}")
    wayland = report["wayland"]
    commands = report["commands"]
    packages = report["packages"]
    terminal_backend = report["terminal_backend"]
    print(f"wayland: ok={wayland['ok']} WAYLAND_DISPLAY={wayland['WAYLAND_DISPLAY'] or '<unset>'} XDG_SESSION_TYPE={wayland['XDG_SESSION_TYPE'] or '<unset>'}")
    print(
        "terminal_backend:"
        f" active={terminal_backend['active']}"
        f" requested={terminal_backend['requested']}"
        f" supported={','.join(terminal_backend['supported'])}"
    )
    if terminal_backend["note"]:
        print(f"terminal_backend_note: {terminal_backend['note']}")
    print("commands:")
    for name, ok in commands.items():
        print(f"  {name}: {'ok' if ok else 'missing'}")
    print("packages:")
    for name, version in packages.items():
        print(f"  {name}: {version or 'missing'}")
    print(f"ready: {'yes' if report['ready'] else 'no'}")
    if not report["ready"]:
        print("missing:", ", ".join(report["missing"]))
        print("install:", report["install_command"])
    print("run:", report["run_command"])


def main(argv: Optional[list[str]] = None) -> int:
    argv = list(argv or sys.argv[1:])
    json_mode = "--json" in argv
    report = assess_environment()
    if json_mode:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        _print_human(report)
    return 0 if report["ready"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
