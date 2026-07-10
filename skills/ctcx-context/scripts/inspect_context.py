#!/usr/bin/env python3
"""Inventory agent-context sources and evidence for ctcx authoring."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


SKIP_DIRECTORIES = {
    ".git",
    ".next",
    ".turbo",
    ".venv",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "target",
    "vendor",
}
ROOT_CLUES = (
    "Cargo.toml",
    "Makefile",
    "Justfile",
    "Taskfile.yml",
    "Taskfile.yaml",
    "package.json",
    "pnpm-workspace.yaml",
    "pyproject.toml",
    "go.mod",
    "Gemfile",
    "composer.json",
    "Dockerfile",
    "docker-compose.yml",
    "docker-compose.yaml",
    "CONTRIBUTING.md",
    "README.md",
)
INSTRUCTION_NAMES = {"AGENTS.md", "CLAUDE.md", "CLAUDE.local.md"}


def relative(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


def is_ignored(path: Path) -> bool:
    return any(part in SKIP_DIRECTORIES for part in path.parts)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def line_count(path: Path) -> int:
    return len(path.read_text(encoding="utf-8", errors="replace").splitlines())


def collect(root: Path) -> dict[str, object]:
    instruction_files: list[dict[str, object]] = []
    claude_files: list[str] = []
    workflows: list[str] = []

    for path in sorted(root.rglob("*")):
        if not path.is_file() or is_ignored(path.relative_to(root)):
            continue
        rel = relative(root, path)
        if path.name in INSTRUCTION_NAMES:
            instruction_files.append(
                {
                    "path": rel,
                    "kind": path.name,
                    "lines": line_count(path),
                    "sha256": digest(path),
                }
            )
        elif ".claude" in path.parts:
            claude_files.append(rel)
        elif path.parts[:2] == (".github", "workflows"):
            workflows.append(rel)

    root_clues = [name for name in ROOT_CLUES if (root / name).is_file()]
    ctcx_config = root / "ctcx.yaml"
    return {
        "root": str(root),
        "ctcx": {
            "config_exists": ctcx_config.is_file(),
            "config_path": "ctcx.yaml" if ctcx_config.is_file() else None,
        },
        "instruction_files": instruction_files,
        "claude_support_files": claude_files,
        "project_evidence": {
            "root_files": root_clues,
            "github_workflows": workflows,
            "top_level_directories": sorted(
                path.name
                for path in root.iterdir()
                if path.is_dir() and path.name not in SKIP_DIRECTORIES
            ),
        },
    }


def markdown(report: dict[str, object]) -> str:
    ctcx = report["ctcx"]
    evidence = report["project_evidence"]
    lines = [f"# ctcx context inventory: `{report['root']}`", ""]
    lines.append(f"- Existing ctcx config: {'yes' if ctcx['config_exists'] else 'no'}")

    instruction_files = report["instruction_files"]
    if instruction_files:
        lines.extend(["", "## Instruction files"])
        for item in instruction_files:
            lines.append(
                f"- `{item['path']}` ({item['kind']}, {item['lines']} lines, sha256 {item['sha256']})"
            )
    else:
        lines.extend(["", "## Instruction files", "- None found."])

    if report["claude_support_files"]:
        lines.extend(["", "## .claude support files"])
        lines.extend(f"- `{path}`" for path in report["claude_support_files"])

    lines.extend(["", "## Project evidence"])
    lines.append(
        "- Root files: "
        + (", ".join(f"`{path}`" for path in evidence["root_files"]) or "none")
    )
    lines.append(
        "- CI workflows: "
        + (", ".join(f"`{path}`" for path in evidence["github_workflows"]) or "none")
    )
    lines.append(
        "- Top-level directories: "
        + (", ".join(f"`{path}`" for path in evidence["top_level_directories"]) or "none")
    )
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", default=".", help="repository root to inspect")
    parser.add_argument(
        "--format", choices=("json", "markdown"), default="json", help="report format"
    )
    args = parser.parse_args()
    root = Path(args.root).expanduser().resolve()
    if not root.is_dir():
        parser.error(f"repository root is not a directory: {root}")

    report = collect(root)
    if args.format == "markdown":
        print(markdown(report), end="")
    else:
        print(json.dumps(report, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
