# ctcx authoring reference

Use this as the compact schema reference while authoring `ctcx.yaml` and imported YAML fragments. `ctcx` currently accepts schema version `1` and rejects unknown fields.

## Minimal project

```yaml
version: 1

project:
  name: example

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

rules:
  - id: shared-workflow
    targets: ["*"]
    section: workflow
    order: 100
    content:
      file: instructions/workflow.md
```

`project.name`, `outputs`, `sections`, and `rules` are root-only. Imported fragments contain `version`, optional `imports`, `sections`, and `rules`.

## Fields

| Field | Meaning |
| --- | --- |
| `imports: [{ path: context/rust.yaml }]` | Include a local YAML fragment. Imports resolve relative to the declaring YAML and cannot leave the project root. |
| `outputs.<target>.path` | Generated output path relative to project root. |
| `outputs.<target>.title` | H1 title of that generated output. |
| `sections[].id` | Stable identifier referenced by `rule.section`. |
| `sections[].order` | Integer ordering; default `1000`. |
| `rules[].id` | Globally unique stable rule identifier. |
| `rules[].targets` | Output target IDs, or `"*"` for all outputs. |
| `rules[].content.inline` | Short inline Markdown text. |
| `rules[].content.file` | Markdown source path relative to the YAML file declaring it. |
| `rules[].slot` | Optional mutually exclusive category. |
| `rules[].priority` | Higher value wins inside the same slot for a target; equal values are an error. |
| `rules[].order` | Integer ordering inside a section; default `1000`. |

Imports, Markdown files, and `ctcx.yaml` are source dependencies. Their content changes make generated output stale. The generated manifest records provenance and output hashes.

## Target-specific replacement

Use a slot only when one rule replaces another for a target:

```yaml
rules:
  - id: default-package-manager
    slot: tooling.package-manager
    priority: 100
    targets: ["*"]
    section: workflow
    order: 10
    content:
      inline: Use the repository package manager.

  - id: claude-package-manager
    slot: tooling.package-manager
    priority: 200
    targets: [claude]
    section: workflow
    order: 10
    content:
      inline: Use Cargo directly. Do not add command wrappers.
```

Without `slot`, both rules are additive. `priority` has no effect unless rules share a slot for the same target.

## Guardrails

Attach checks to the rule whose instruction they support. Declare only checks that `ctcx` can verify explicitly:

```yaml
rules:
  - id: verify-before-submit
    targets: ["*"]
    section: workflow
    content:
      inline: Run the test suite before submitting a change.
    checks:
      - type: package-script
        manifest: package.json
        script: test
      - type: path-exists
        path: scripts/setup.sh
        kind: file
```

`package-script` requires a JSON package manifest with a string script entry. `path-exists.kind` is `any`, `file`, or `directory` and defaults to `any`. All paths are project-root-relative, even in imported fragments. `ctcx` does not validate arbitrary shell commands, Cargo targets, or runtime executables.

Checks run only for targets where their rule is effective; a check on a suppressed rule does not run. Use `ctcx explain` to understand a rule's effective targets.

## Lifecycle

```sh
ctcx validate                # compile and validate without writing
ctcx diff                    # inspect expected changes
ctcx build                   # write generated outputs and manifest
ctcx check                   # ensure nothing is stale or hand-edited
ctcx explain --target agents # inspect rule provenance
```

`build` protects untracked and manually edited output files. Use `--force` only after reviewing the migration diff. `check` detects missing, stale, modified, and obsolete generated state.
