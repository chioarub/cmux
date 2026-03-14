#!/usr/bin/env python3
"""Unit test for the cross-platform capability matrix fixtures."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from capability_matrix import (  # type: ignore[import-not-found]
    load_capability_fixture,
    load_capability_matrix,
    method_contract_status,
    supported_test_groups,
)


def _must(cond: bool, msg: str) -> None:
    if not cond:
        raise AssertionError(msg)


def main() -> int:
    matrix = load_capability_matrix()
    linux = load_capability_fixture("linux-wayland-v1")
    macos = load_capability_fixture("macos")

    _must(matrix.get("version") == 1, f"Unexpected matrix version: {matrix}")
    _must(method_contract_status(linux, "window.list") == "supported", "linux should support window.list")
    _must(method_contract_status(linux, "window.create") == "supported", "linux should support window.create")
    _must(method_contract_status(linux, "browser.navigate") == "unsupported", "linux should advertise but reject browser.navigate")
    _must(method_contract_status(macos, "browser.navigate") == "supported", "macOS should support browser.navigate")
    _must(
        ((linux.get("terminal_backend") or {}).get("active") == "vte"),
        f"linux should advertise the vte terminal backend: {linux.get('terminal_backend')}",
    )

    linux_groups = supported_test_groups(matrix, linux)
    macos_groups = supported_test_groups(matrix, macos)

    _must("core" in linux_groups, f"linux core group missing: {linux_groups}")
    _must("browser" not in linux_groups, f"linux should not opt into browser group: {linux_groups}")
    _must("window_multi" in linux_groups, f"linux window_multi group missing: {linux_groups}")
    _must("browser" in macos_groups, f"macOS browser group missing: {macos_groups}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
