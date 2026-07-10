# ctcx

[![CI](https://github.com/benkleyner/ctcx/actions/workflows/ci.yml/badge.svg)](https://github.com/benkleyner/ctcx/actions/workflows/ci.yml)
[![License: MIT or Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

`ctcx` compiles structured YAML rules into deterministic Markdown context files for coding agents. A project can generate root or nested `AGENTS.md`, `CLAUDE.md`, and other named outputs from one source of truth.

## Install

```bash
cargo install --path .
```

Or install the latest release with Homebrew:

```bash
brew install benkleyner/tap/ctcx
```

Initialize a project and verify the generated files:

```bash
ctcx init
ctcx validate
ctcx check
```

## Configuration

The nearest `ctcx.yaml` in the current directory or its parents is used. Pass `--config <path>` to select one explicitly.

```yaml
version: 1

project:
  name: example

imports:
  - path: context/rust.yaml

outputs:
  agents:
    path: AGENTS.md
    title: Project Agent Instructions
  claude:
    path: CLAUDE.md
    title: Claude Code Instructions

sections:
  - id: workflow
    title: Workflow
    order: 100
  - id: testing
    title: Testing
    order: 200

rules:
  - id: default-package-manager
    slot: tooling.package-manager
    priority: 100
    targets: ["*"]
    section: workflow
    order: 10
    content:
      inline: |
        Use Cargo for project commands.

  - id: claude-package-manager
    slot: tooling.package-manager
    priority: 200
    targets: [claude]
    section: workflow
    order: 10
    content:
      inline: |
        Use Cargo directly. Do not add command wrappers.

  - id: testing-guide
    targets: [agents, claude]
    section: testing
    content:
      file: instructions/testing.md
```

Imported YAML fragments have the following shape and may recursively import other fragments:

```yaml
version: 1
imports: []
sections: []
rules: []
```

All imports are local, resolved relative to the YAML file that declares them, and confined to the project root. `ctcx` reports the full path when it detects an import cycle.

## Guardrail checks

Rules can declare explicit checks for project state referenced by their instructions:

```yaml
rules:
  - id: test-workflow
    targets: [agents, claude]
    section: testing
    content:
      inline: Run `bun test` before submitting changes.
    checks:
      - type: package-script
        manifest: package.json
        script: test
      - type: path-exists
        path: scripts/setup.sh
        kind: file
```

Check paths are relative to the project root, including checks declared in imported fragments. A `package-script` check requires a JSON package manifest with a string-valued entry in its `scripts` object. A `path-exists` check accepts `kind: any`, `file`, or `directory`; the default is `any`. Absolute paths, paths that escape the project root, and symlinks that resolve outside it are rejected.

Checks run after rule precedence is resolved. Each check must pass for every target where its rule is effective; checks on suppressed rules do not run for that target. Failures from all effective rules and targets are reported together. Checked project files are revalidated by each command but are not added to the generated manifest because their contents do not affect the rendered Markdown.

Checks are explicit contracts. `ctcx` does not infer commands from Markdown or validate arbitrary executables, shell syntax, Cargo targets, Make targets, or runtime `PATH` availability. Bun, npm, pnpm, and Yarn scripts all use the same package-manifest check.

## Conditional rules

An optional `when` expression determines whether a rule is eligible before target selection, slot precedence, rendering, and guardrail checks. Conditions are deterministic filesystem facts evaluated relative to the project root:

```yaml
rules:
  - id: rust-workflow
    targets: ["*"]
    section: workflow
    content:
      inline: Use Cargo for project commands.
    when:
      type: all
      conditions:
        - type: path-exists
          path: Cargo.toml
          kind: file
        - type: any
          conditions:
            - type: path-exists
              path: src
              kind: directory
            - type: not
              condition:
                type: path-exists
                path: legacy
```

`all` and `any` require at least one nested condition. `not` accepts exactly one. `path-exists` accepts a project-root-relative `path` and an optional `kind` of `any` (the default), `file`, or `directory`. Use `not` with `path-exists` to require that a path is absent.

Every nested condition is evaluated, so invalid paths and symlinks resolving outside the project root always fail safely. A false rule is inapplicable: it cannot win or suppress a slot and its checks do not run. The final Boolean result for every rule is included in the source fingerprint; a filesystem change that changes rule eligibility makes `ctcx check` report stale generated context. Use `ctcx explain` to see inapplicable rules.

## Rule precedence

Rules are additive by default. Rules become mutually exclusive when they use the same `slot` for the same target. The highest numeric `priority` wins; a tie is a validation error.

Priority affects selection only. Emitted rules are sorted by section order, rule order, and rule ID. Import order never changes generated output.

Use `ctcx explain` to inspect the decision:

```bash
ctcx explain --target claude
ctcx explain --target claude --slot tooling.package-manager
ctcx explain --rule default-package-manager
```

## Commands

| Command | Purpose |
| --- | --- |
| `ctcx init` | Scaffold a configuration and initial generated files. |
| `ctcx validate` | Resolve and compile the complete project without writing. |
| `ctcx build` | Generate every configured output and the manifest. |
| `ctcx compile` | Alias for `ctcx build`. |
| `ctcx check` | Fail when generated state is missing, stale, modified, or obsolete. |
| `ctcx diff` | Print unified differences without writing files. |
| `ctcx explain` | Show rule provenance and precedence decisions. |

`build` refuses to overwrite manually edited or untracked destinations. Use `--force` only when those edits should be replaced. `build --dry-run` reports what would change.

## Generated-state checks

Successful builds write `.ctcx/manifest.yaml`. It records source hashes, output hashes, compiler metadata, applied rules, and suppressed rules. Commit the manifest with generated context files, then enforce this in CI:

```bash
ctcx validate
ctcx check
```

When sources change, `check` reports stale outputs. When a generated file is edited directly, it reports a modified output. If both happen, it reports both conditions together.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) for community expectations. Security vulnerabilities should be reported according to [SECURITY.md](SECURITY.md).

## License

Licensed under either of the following, at your option:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project is licensed under those terms, without additional conditions.
