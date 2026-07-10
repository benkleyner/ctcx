---
name: ctcx-context
description: Configure, migrate, and maintain ctcx-managed agent instruction files. Use when a repository needs to turn existing AGENTS.md, CLAUDE.md, nested agent instructions, or project conventions into deterministic ctcx YAML; when it needs new useful agent instructions derived from its codebase; or when generated agent context has drifted and must be updated safely.
---

# ctcx Context

Manage agent instructions as compiled project state. Keep human-authored guidance in ctcx YAML and Markdown source files; never hand-edit a generated output.

## Discover first

1. Run `scripts/inspect_context.py --root <repo> --format markdown` from this skill. Use its inventory as a starting point, then read every discovered instruction file and the project files that support a proposed rule.
2. Check for `ctcx.yaml` before scaffolding. If it exists, inspect it, its imports, and `ctcx check`; preserve its target names, paths, and source layout unless a change is required.
3. Read repository-local guidance before inferring rules: `CONTRIBUTING.md`, build manifests, task runners, CI workflows, formatter/linter configuration, and relevant source directories. Treat every existing `AGENTS.md` or `CLAUDE.md` as a migration source, including nested files.
4. Establish a working compiler command. Prefer `ctcx`; in a checkout of the compiler use `cargo run --`. If it is absent elsewhere, install the released compiler with `cargo install ctcx` or use the project's documented installation method.

Do not infer commands only from prose. Confirm them in a manifest, task runner, CI, or repository documentation. Do not copy stale generated headers, compiler warnings, or user-specific local settings into shared instructions.

## Choose the path

### Existing ctcx project

Edit source YAML and its Markdown fragments, then run `ctcx validate`, `ctcx diff`, and `ctcx build`. Use `ctcx explain` before changing `slot` or `priority` behavior. Finish with `ctcx check`.

### Existing instruction files, no ctcx project

1. Translate the files into source-owned Markdown fragments under `instructions/` (and optional imported YAML fragments under `context/`). Keep claims equivalent, but remove generated banners and resolve duplicated or contradictory wording deliberately.
2. Write `ctcx.yaml` with an output for every file that should remain generated. Use target IDs that identify the harness, such as `agents` and `claude`; an output path may be nested.
3. Run `ctcx validate` and `ctcx diff`. Compare each expected output to its legacy file before replacing it.
4. Only after the ctcx source preserves the intended behavior, run `ctcx build --force` to adopt existing untracked outputs. Commit the source files, generated files, and `.ctcx/manifest.yaml` together.

`ctcx init` is appropriate only when no destination it would create already exists. It refuses to initialize over existing `AGENTS.md` or `CLAUDE.md`; do not use `--force` as a migration shortcut.

### No instruction files and no ctcx project

Run `ctcx init`, then replace its placeholder instruction with concise, evidence-backed guidance. Start with only the rules that reduce common agent mistakes:

- project shape and where to look first;
- the verified install, format/lint, test, build, and local-run commands;
- repository-specific contribution, safety, generated-file, and validation constraints;
- target-specific differences only when an agent harness actually needs different wording.

Do not invent an architecture, deployment process, test command, or policy. Omit unknown facts and leave a narrow instruction rather than generic boilerplate. Prefer a single root `AGENTS.md` and `CLAUDE.md`; add nested outputs only when the repository has a real scoped convention that requires them.

After replacing the placeholder source, run `ctcx validate`, `ctcx diff`, `ctcx build`, and `ctcx check` before considering setup complete. `init` has already generated the outputs and manifest, so they are stale until the build runs again.

## Author ctcx sources

Read [references/ctcx-schema.md](references/ctcx-schema.md) before authoring or restructuring YAML.

Use this mapping:

| Source material | ctcx representation |
| --- | --- |
| Shared instruction | `targets: ["*"]` rule |
| Harness-specific wording | target-specific rule, optionally sharing a `slot` with its replacement |
| Long or independently maintained guidance | `content.file` Markdown fragment |
| Reusable category of rules | imported YAML fragment |
| Requirement tied to a known project file or package script | explicit `checks` entry |

Use `slot` only for mutually exclusive rules. Give competing rules distinct numeric `priority` values. Keep output order stable with `section.order` and `rule.order`; do not rely on import order. Keep source Markdown factual, imperative, and specific enough to guide an agent.

## Maintain safely

Make all edits in ctcx sources, then use this loop:

```sh
ctcx validate
ctcx diff
ctcx build
ctcx check
```

Use `ctcx check` in CI after `ctcx validate`. If generated content changes unexpectedly, use `ctcx diff` to see it and `ctcx explain --target <target>` or `ctcx explain --rule <rule-id>` to trace its provenance. Use `ctcx build --force` only to intentionally replace a manually edited or legacy output after reviewing the diff.

Keep `.ctcx/manifest.yaml` and every configured output committed. Do not edit them directly. When a command, workflow, or project policy changes, update the supporting source fragment and guardrail checks in the same change.
