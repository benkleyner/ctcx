use anyhow::Result;
use ctcx::{
    BuildSafety, build_project, check_project, compile_project, init_project, load_project,
    render_diffs,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn fixture() -> (TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("ctcx.yaml");
    write(
        &config,
        r#"version: 1
project:
  name: fixture
imports:
  - path: context/common.yaml
outputs:
  agents:
    path: AGENTS.md
    title: Agent Instructions
  claude:
    path: CLAUDE.md
    title: Claude Instructions
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
    order: 20
    content:
      inline: Use Cargo by default.
  - id: claude-package-manager
    slot: tooling.package-manager
    priority: 200
    targets: [claude]
    section: workflow
    order: 20
    content:
      inline: Use Cargo directly for Claude.
"#,
    );
    write(
        &temp.path().join("context/common.yaml"),
        r#"version: 1
rules:
  - id: test-guide
    targets: ["*"]
    section: testing
    order: 10
    content:
      file: ../instructions/testing.md
"#,
    );
    write(
        &temp.path().join("instructions/testing.md"),
        "Run the complete test suite.\r\n",
    );
    (temp, config)
}

#[test]
fn resolves_precedence_markdown_imports_and_ordering() -> Result<()> {
    let (_temp, config) = fixture();
    let project = load_project(&config)?;
    let compiled = compile_project(&project)?;
    let agents = &compiled.outputs["agents"];
    let claude = &compiled.outputs["claude"];

    assert!(agents.content.contains("Use Cargo by default."));
    assert!(!agents.content.contains("Use Cargo directly for Claude."));
    assert!(claude.content.contains("Use Cargo directly for Claude."));
    assert!(!claude.content.contains("Use Cargo by default."));
    assert!(claude.content.contains("Run the complete test suite.\n"));
    assert!(claude.content.find("## Workflow") < claude.content.find("## Testing"));
    assert_eq!(claude.suppressed[0].rule, "default-package-manager");
    assert_eq!(claude.suppressed[0].winner, "claude-package-manager");
    assert_eq!(project.dependencies.len(), 3);
    Ok(())
}

#[test]
fn equal_priority_slot_conflicts_are_errors() -> Result<()> {
    let (temp, config) = fixture();
    let text = fs::read_to_string(&config)?.replace("priority: 200", "priority: 100");
    write(&config, &text);
    let project = load_project(&config)?;
    let error = compile_project(&project).unwrap_err().to_string();
    assert!(error.contains("equal-priority rules"));
    assert!(error.contains("tooling.package-manager"));
    drop(temp);
    Ok(())
}

#[test]
fn reports_complete_import_cycles() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("ctcx.yaml");
    write(
        &config,
        r#"version: 1
project: { name: cycle }
imports: [{ path: a.yaml }]
outputs:
  agents: { path: AGENTS.md, title: Agents }
sections: [{ id: workflow, title: Workflow }]
rules:
  - id: root-rule
    targets: ["*"]
    section: workflow
    content: { inline: Root }
"#,
    );
    write(
        &temp.path().join("a.yaml"),
        "version: 1\nimports: [{ path: b.yaml }]\n",
    );
    write(
        &temp.path().join("b.yaml"),
        "version: 1\nimports: [{ path: a.yaml }]\n",
    );

    let error = load_project(&config).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("import cycle detected"));
    assert!(message.contains("a.yaml"));
    assert!(message.contains("b.yaml"));
}

#[cfg(unix)]
#[test]
fn detects_cycles_through_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("ctcx.yaml");
    write(
        &config,
        r#"version: 1
project: { name: cycle }
imports: [{ path: fragment.yaml }]
outputs:
  agents: { path: AGENTS.md, title: Agents }
sections: [{ id: workflow, title: Workflow }]
rules:
  - id: root-rule
    targets: ["*"]
    section: workflow
    content: { inline: Root }
"#,
    );
    write(
        &temp.path().join("fragment.yaml"),
        "version: 1\nimports: [{ path: root-link.yaml }]\n",
    );
    symlink(&config, temp.path().join("root-link.yaml")).unwrap();
    assert!(format!("{:#}", load_project(&config).unwrap_err()).contains("import cycle detected"));
}

#[test]
fn build_check_diff_and_force_cover_the_drift_lifecycle() -> Result<()> {
    let (temp, config) = fixture();
    let project = load_project(&config)?;
    let compiled = compile_project(&project)?;
    build_project(&project, &compiled, BuildSafety::Safe, false)?;
    assert!(check_project(&project, &compiled, None)?.is_clean());
    assert!(render_diffs(&project, &compiled, None)?.is_empty());

    write(
        &temp.path().join("instructions/testing.md"),
        "Run tests and clippy.\n",
    );
    let changed_project = load_project(&config)?;
    let changed_compiled = compile_project(&changed_project)?;
    let stale = check_project(&changed_project, &changed_compiled, None)?.to_string();
    assert!(stale.contains("stale output agents"));
    assert!(!render_diffs(&changed_project, &changed_compiled, None)?.is_empty());

    write(&temp.path().join("AGENTS.md"), "manual edit\n");
    let both = check_project(&changed_project, &changed_compiled, Some("agents"))?.to_string();
    assert!(both.contains("stale and modified output agents"));
    let refusal = build_project(
        &changed_project,
        &changed_compiled,
        BuildSafety::Safe,
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(refusal.contains("refusing to overwrite modified output"));

    build_project(
        &changed_project,
        &changed_compiled,
        BuildSafety::Force,
        false,
    )?;
    assert!(check_project(&changed_project, &changed_compiled, None)?.is_clean());
    Ok(())
}

#[test]
fn build_removes_only_safe_obsolete_outputs() -> Result<()> {
    let (temp, config) = fixture();
    let project = load_project(&config)?;
    let compiled = compile_project(&project)?;
    build_project(&project, &compiled, BuildSafety::Safe, false)?;

    let text = fs::read_to_string(&config)?;
    let start = text.find("  claude:\n").unwrap();
    let end = text[start..].find("sections:\n").unwrap() + start;
    let without_claude_output = format!("{}{}", &text[..start], &text[end..]);
    let rule_start = without_claude_output
        .find("  - id: claude-package-manager\n")
        .unwrap();
    let without_claude = &without_claude_output[..rule_start];
    write(&config, without_claude);
    let updated = load_project(&config)?;
    let updated_compiled = compile_project(&updated)?;
    build_project(&updated, &updated_compiled, BuildSafety::Safe, false)?;
    assert!(!temp.path().join("CLAUDE.md").exists());
    assert!(check_project(&updated, &updated_compiled, None)?.is_clean());
    Ok(())
}

#[test]
fn rejects_unknown_fields_multi_document_yaml_and_non_yaml_configs() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("ctcx.yaml");
    write(&config, "version: 1\nunknown: true\n");
    assert!(format!("{:#}", load_project(&config).unwrap_err()).contains("unknown field"));

    write(&config, "version: 1\n---\nversion: 1\n");
    assert!(format!("{:#}", load_project(&config).unwrap_err()).contains("multiple documents"));

    let json = temp.path().join("ctcx.json");
    write(&json, "{}\n");
    assert!(
        load_project(&json)
            .unwrap_err()
            .to_string()
            .contains("must be a YAML file")
    );
}

#[test]
fn rejects_imports_and_outputs_that_escape_the_project_root() {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("project");
    fs::create_dir_all(&root).unwrap();
    write(&outer.path().join("outside.yaml"), "version: 1\n");
    let config = root.join("ctcx.yaml");
    write(
        &config,
        r#"version: 1
project: { name: paths }
imports: [{ path: ../outside.yaml }]
outputs:
  agents: { path: AGENTS.md, title: Agents }
sections: [{ id: workflow, title: Workflow }]
rules:
  - id: root-rule
    targets: ["*"]
    section: workflow
    content: { inline: Root }
"#,
    );
    assert!(format!("{:#}", load_project(&config).unwrap_err()).contains("escapes project root"));

    write(
        &config,
        r#"version: 1
project: { name: paths }
outputs:
  agents: { path: ../AGENTS.md, title: Agents }
sections: [{ id: workflow, title: Workflow }]
rules:
  - id: root-rule
    targets: ["*"]
    section: workflow
    content: { inline: Root }
"#,
    );
    assert!(
        format!("{:#}", load_project(&config).unwrap_err()).contains("escapes the project root")
    );
}

#[test]
fn init_scaffolds_a_clean_checked_project_and_refuses_collisions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = temp.path().join("ctcx.yaml");
    let written = init_project(&config, false)?;
    assert_eq!(written.len(), 5);
    assert!(temp.path().join("AGENTS.md").exists());
    assert!(temp.path().join("CLAUDE.md").exists());
    assert!(temp.path().join(".ctcx/manifest.yaml").exists());

    let project = load_project(&config)?;
    let compiled = compile_project(&project)?;
    assert!(check_project(&project, &compiled, None)?.is_clean());
    assert!(
        init_project(&config, false)
            .unwrap_err()
            .to_string()
            .contains("refusing to initialize")
    );
    Ok(())
}

#[test]
fn check_detects_a_manually_modified_manifest() -> Result<()> {
    let (temp, config) = fixture();
    let project = load_project(&config)?;
    let compiled = compile_project(&project)?;
    build_project(&project, &compiled, BuildSafety::Safe, false)?;

    let manifest_path = temp.path().join(".ctcx/manifest.yaml");
    let manifest = fs::read_to_string(&manifest_path)?;
    write(
        &manifest_path,
        &manifest.replace(
            "rules:\n    - default-package-manager",
            "rules:\n    - made-up-rule",
        ),
    );
    let report = check_project(&project, &compiled, None)?.to_string();
    assert!(report.contains("manifest contents do not match"));
    Ok(())
}
