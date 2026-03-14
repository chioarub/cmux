#!/usr/bin/env python3
"""Helpers for the shared cross-platform socket capability matrix."""

from __future__ import annotations

import json
from functools import lru_cache
from pathlib import Path
from typing import Any, Dict, Set


ROOT = Path(__file__).resolve().parent.parent
FIXTURES_DIR = ROOT / "fixtures" / "socket-v2"


def _load_json(path: Path) -> Dict[str, Any]:
    with path.open("r", encoding="utf-8") as handle:
        payload = json.load(handle)
    if not isinstance(payload, dict):
        raise ValueError(f"Expected JSON object in {path}")
    return payload


@lru_cache(maxsize=1)
def load_capability_matrix() -> Dict[str, Any]:
    return _load_json(FIXTURES_DIR / "capability-matrix.json")


def load_capability_fixture(name: str) -> Dict[str, Any]:
    return _load_json(FIXTURES_DIR / f"system.capabilities.{name}.json")


def _platform_id(capabilities: Dict[str, Any]) -> str:
    platform = capabilities.get("platform") or {}
    if not isinstance(platform, dict):
        return ""
    value = platform.get("id")
    return str(value or "").strip()


def _feature_flags(capabilities: Dict[str, Any]) -> Dict[str, bool]:
    raw = capabilities.get("features") or {}
    if not isinstance(raw, dict):
        return {}
    return {str(key): bool(value) for key, value in raw.items()}


def _advertised_methods(capabilities: Dict[str, Any]) -> Set[str]:
    return {str(method) for method in (capabilities.get("methods") or [])}


def _unsupported_methods(capabilities: Dict[str, Any], matrix: Dict[str, Any]) -> Set[str]:
    methods = {str(method) for method in (capabilities.get("unsupported_methods") or [])}
    groups = matrix.get("method_groups") or {}
    for group_name in capabilities.get("unsupported_method_groups") or []:
        for method in groups.get(str(group_name), []):
            methods.add(str(method))
    return methods


def method_contract_status(capabilities: Dict[str, Any], method: str) -> str:
    matrix = load_capability_matrix()
    if method in _unsupported_methods(capabilities, matrix):
        return "unsupported"
    if method in _advertised_methods(capabilities):
        return "supported"
    return "unadvertised"


def supported_test_groups(matrix: Dict[str, Any], capabilities: Dict[str, Any]) -> Set[str]:
    features = _feature_flags(capabilities)
    platform_id = _platform_id(capabilities)
    supported: Set[str] = set()

    for group_name, raw_group in (matrix.get("test_groups") or {}).items():
        if not isinstance(raw_group, dict):
            continue

        required_features = [str(name) for name in raw_group.get("required_features") or []]
        if any(not features.get(name, False) for name in required_features):
            continue

        allowed_platforms = [str(name) for name in raw_group.get("platform_ids") or []]
        if allowed_platforms and platform_id not in allowed_platforms:
            continue

        supported.add(str(group_name))

    return supported
