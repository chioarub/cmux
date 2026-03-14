#!/usr/bin/env python3
"""Unit test for the Linux Wayland doctor/launcher helper."""

import importlib.util
from pathlib import Path


def _load_module():
    path = Path(__file__).resolve().parent.parent / "scripts" / "linux_wayland_doctor.py"
    spec = importlib.util.spec_from_file_location("linux_wayland_doctor", path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"Unable to load module from {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _must(cond: bool, msg: str) -> None:
    if not cond:
        raise AssertionError(msg)


def main() -> int:
    doctor = _load_module()

    missing = doctor.assess_environment(
        env={},
        command_exists=lambda name: False,
        pkg_config_modversion=lambda name: None,
    )
    _must(missing["ready"] is False, f"expected missing env to be not ready: {missing}")
    _must(missing["wayland"]["ok"] is False, f"expected missing wayland: {missing}")
    _must(
        "sudo pacman -S --needed gtk4 libadwaita vte4 pkgconf rustup clang zig"
        not in missing["install_command"],
        missing["install_command"],
    )
    _must(
        "sudo pacman -S --needed gtk4 libadwaita vte4 pkgconf rustup clang"
        in missing["install_command"],
        missing["install_command"],
    )

    ready = doctor.assess_environment(
        env={"WAYLAND_DISPLAY": "wayland-0", "XDG_SESSION_TYPE": "wayland"},
        command_exists=lambda name: True,
        pkg_config_modversion=lambda name: {
            "gtk4": "4.20.3",
            "libadwaita-1": "1.8.4",
            "vte-2.91-gtk4": "0.82.3",
        }.get(name),
    )
    _must(ready["ready"] is True, f"expected ready env: {ready}")
    _must(
        ready["run_command"].startswith(
            "GDK_BACKEND=wayland cargo run --manifest-path linux/Cargo.toml -p cmux-linux"
        ),
        ready["run_command"],
    )
    _must(ready["terminal_backend"]["active"] == "vte", f"expected vte backend: {ready}")
    _must(
        ready["terminal_backend"]["requested"] is None,
        f"expected fixed vte backend request: {ready}",
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
