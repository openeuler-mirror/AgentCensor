from __future__ import annotations

import copy
import hashlib
import shutil
import tomllib
from pathlib import Path
from typing import Any


MODULES = (
    "preflight",
    "collector",
    "ingest",
    "storage",
    "control",
    "export",
    "memory",
    "e2e",
)


class ConfigError(ValueError):
    pass


def execution_mode(requested: str, module: str) -> str:
    """Select the runner for a module without coupling backend tests to dsh."""
    if module == "e2e":
        if requested == "native":
            raise ConfigError("the e2e module requires dsh; use --mode auto or --mode manual")
        return requested
    return "native"


def module_requires_export(module: str) -> bool:
    return module not in ("preflight", "memory")


def load_scenario(path: Path) -> dict[str, Any]:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise ConfigError(f"cannot read scenario {path}: {error}") from error
    try:
        scenario = tomllib.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise ConfigError(f"invalid scenario {path}: {error}") from error
    validate_scenario(scenario)
    scenario["_meta"] = {
        "path": str(path.resolve()),
        "sha256": hashlib.sha256(raw).hexdigest(),
    }
    return scenario


def validate_scenario(scenario: dict[str, Any]) -> None:
    required_tables = ("benchmark", "agent", "censorscope", "memory", "modules")
    for name in required_tables:
        if not isinstance(scenario.get(name), dict):
            raise ConfigError(f"scenario is missing [{name}]")
    for module in MODULES:
        value = scenario["modules"].get(module)
        if not isinstance(value, dict):
            raise ConfigError(f"scenario is missing [modules.{module}]")
        if module != "preflight" and not isinstance(value.get("workload"), dict):
            raise ConfigError(f"module {module} is missing workload settings")
    interval = scenario["memory"].get("sample_interval_seconds", 1.0)
    if not isinstance(interval, (int, float)) or interval <= 0:
        raise ConfigError("memory.sample_interval_seconds must be positive")


def selected_modules(raw: str) -> list[str]:
    if raw == "all":
        return list(MODULES)
    values = [value.strip() for value in raw.split(",") if value.strip()]
    invalid = sorted(set(values) - set(MODULES))
    if invalid:
        raise ConfigError(f"unknown modules: {', '.join(invalid)}")
    if not values:
        raise ConfigError("at least one module is required")
    return values


def resolve_executable(explicit: str | None, fallback: str, label: str) -> Path:
    candidate = explicit or shutil.which(fallback)
    if not candidate:
        raise ConfigError(f"cannot find {label}; specify its path explicitly")
    path = Path(candidate).expanduser().resolve()
    if not path.is_file():
        raise ConfigError(f"{label} does not exist: {path}")
    return path


def module_cases(scenario: dict[str, Any], module: str) -> list[dict[str, Any]]:
    source = scenario["modules"][module]
    repetitions = int(source.get("repetitions", 1))
    if repetitions < 1:
        raise ConfigError(f"module {module} repetitions must be positive")
    cases = []
    for index in range(1, repetitions + 1):
        case = copy.deepcopy(source)
        case["module"] = module
        case["repetition"] = index
        case["case_id"] = f"{module}-{index:02d}"
        case["seed"] = int(scenario.get("benchmark", {}).get("seed", 42))
        cases.append(case)
    return cases
