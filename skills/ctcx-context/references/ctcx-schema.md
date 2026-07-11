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
    format: agents
  claude:
    path: CLAUDE.md
    title: Claude Code Instructions
    format: claude

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
| `outputs.<target>.format` | Optional renderer: `markdown` (default), `agents`, `claude`, `cursor`, `copilot`, `windsurf`, `cline`, or `template`. |
| `outputs.<target>.front_matter` | Optional YAML mapping emitted before the generated header. Renderer-specific metadata is checked while other keys remain allowed. |
| `outputs.<target>.header` | Optional generated-header policy: `mode: default`, `omit`, or `template` with an inline or file template. |
| `outputs.<target>.templates.output` | Optional inline/file document template; it must contain `{{sections}}` exactly once. Required for `format: template`. |
| `outputs.<target>.templates.section` | Optional inline/file section template; it must contain `{{content}}` exactly once. Required for `format: template`. |
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

## Renderers and templates

`agents` accepts any output named `AGENTS.md`; `claude` accepts any `CLAUDE.md`. The other first-class renderers require their documented project locations: Cursor `.cursor/rules/**/*.mdc`, Copilot `.github/copilot-instructions.md` or `.github/instructions/**/*.instructions.md`, Windsurf `.windsurf/rules/**/*.md`, and Cline `.clinerules/**/*.md` or `.txt`. `markdown` and `template` accept any safe project-relative destination.

Front matter is a YAML mapping and remains extensible. Cursor validates `description` (string), `globs` (string or list), and `alwaysApply` (Boolean). Path-specific Copilot files require a non-empty `applyTo` string. Windsurf requires `trigger: always_on | glob | model_decision | manual`; `glob` requires `globs`, while `model_decision` requires `description`. Cline validates optional `paths` as a string or list.

Templates accept either `inline` or `file`; template files may use any extension, resolve relative to `ctcx.yaml`, must remain inside the project root, and are tracked as source dependencies. The strict placeholders available to output templates are `project_name`, `output_name`, `output_path`, `title`, `renderer`, `fingerprint`, `front_matter`, `generated_header`, and `sections`. Section templates additionally receive `section_id`, `section_title`, `section_order`, and `content`.

```yaml
outputs:
  cursor:
    path: .cursor/rules/project.mdc
    title: Project rules
    format: cursor
    front_matter:
      description: Project workflow
      alwaysApply: true

  custom:
    path: generated/context.txt
    title: Custom context
    format: template
    header: { mode: omit }
    templates:
      output: { file: templates/output.txt }
      section: { inline: "[{{section_title}}] {{content}}" }
```

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
ctcx graph --format json      # inspect the versioned dependency graph
ctcx why AGENTS.md --line 42  # trace a generated line to its source
```

`build` protects untracked and manually edited output files. Use `--force` only after reviewing the migration diff. `check` detects missing, stale, modified, and obsolete generated state.
