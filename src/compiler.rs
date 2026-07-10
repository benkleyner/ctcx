use crate::model::{
    MANIFEST_VERSION, Manifest, ManifestDependency, ManifestOutput, ManifestSuppressed, RawCheck,
    RawDocument, RawPathKind, RawRule, SCHEMA_VERSION,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use similar::TextDiff;
use std::collections::{BTreeMap, HashSet};
use std::fmt::{self, Write as _};
use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use tempfile::NamedTempFile;

const MANIFEST_PATH: &str = ".ctcx/manifest.yaml";

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub name: String,
    pub outputs: BTreeMap<String, Output>,
    pub sections: BTreeMap<String, Section>,
    pub rules: Vec<Rule>,
    pub dependencies: BTreeMap<String, Dependency>,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub order: i32,
    pub source: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub slot: Option<String>,
    pub priority: i32,
    pub targets: Vec<String>,
    pub section: String,
    pub order: i32,
    pub content: String,
    pub source: PathBuf,
    pub content_source: Option<PathBuf>,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    PackageScript { manifest: PathBuf, script: String },
    PathExists { path: PathBuf, kind: PathKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Any,
    File,
    Directory,
}

#[derive(Debug, Clone)]
pub struct Dependency {
    pub path: PathBuf,
    pub relative_path: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct CompiledProject {
    pub source_fingerprint: String,
    pub outputs: BTreeMap<String, CompiledOutput>,
    pub manifest: Manifest,
}

#[derive(Debug, Clone)]
pub struct CompiledOutput {
    pub name: String,
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub content: String,
    pub sha256: String,
    pub applied: Vec<String>,
    pub suppressed: Vec<Suppression>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suppression {
    pub rule: String,
    pub winner: String,
    pub slot: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildSafety {
    Safe,
    Force,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplainStatus {
    Applied,
    Suppressed { winner: String },
    NotTargeted,
}

#[derive(Debug, Default, Clone)]
pub struct DriftReport {
    issues: Vec<String>,
}

impl DriftReport {
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    fn push(&mut self, issue: impl Into<String>) {
        self.issues.push(issue.into());
    }
}

impl fmt::Display for DriftReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(formatter, "generated context drift detected:")?;
        for issue in &self.issues {
            writeln!(formatter, "  - {issue}")?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Loader {
    root: PathBuf,
    config_path: PathBuf,
    documents: Vec<(PathBuf, RawDocument)>,
    visiting: Vec<PathBuf>,
    visited: HashSet<PathBuf>,
    dependencies: BTreeMap<String, Dependency>,
}

pub fn discover_config(start: &Path) -> Result<PathBuf> {
    let mut directory = if start.is_file() {
        start.parent().unwrap_or(start).to_path_buf()
    } else {
        start.to_path_buf()
    };

    loop {
        let candidate = directory.join("ctcx.yaml");
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .with_context(|| format!("failed to resolve {}", candidate.display()));
        }
        if !directory.pop() {
            break;
        }
    }

    bail!(
        "could not find ctcx.yaml from {} or any parent directory",
        start.display()
    )
}

pub fn load_project(config_path: &Path) -> Result<Project> {
    ensure_yaml_extension(config_path, "configuration")?;
    let config_path = config_path
        .canonicalize()
        .with_context(|| format!("failed to resolve configuration {}", config_path.display()))?;
    let root = config_path
        .parent()
        .context("configuration path has no parent directory")?
        .canonicalize()
        .context("failed to resolve project root")?;

    let mut loader = Loader {
        root: root.clone(),
        config_path: config_path.clone(),
        ..Loader::default()
    };
    loader.visit(&config_path, true)?;
    loader.finish()
}

impl Loader {
    fn visit(&mut self, path: &Path, is_root: bool) -> Result<()> {
        ensure_yaml_extension(path, "import")?;
        let canonical = path
            .canonicalize()
            .with_context(|| format!("failed to resolve YAML source {}", path.display()))?;
        ensure_inside(&self.root, &canonical, "import")?;

        if let Some(index) = self.visiting.iter().position(|item| item == &canonical) {
            let mut chain = self.visiting[index..]
                .iter()
                .map(|item| display_relative(&self.root, item))
                .collect::<Vec<_>>();
            chain.push(display_relative(&self.root, &canonical));
            bail!("import cycle detected:\n  {}", chain.join("\n  -> "));
        }
        if self.visited.contains(&canonical) {
            return Ok(());
        }

        let bytes = fs::read(&canonical)
            .with_context(|| format!("failed to read YAML source {}", canonical.display()))?;
        let text = String::from_utf8(bytes.clone())
            .map_err(|_| anyhow!("YAML source {} is not valid UTF-8", canonical.display()))?;
        let document = parse_yaml_document(&canonical, &text)?;
        ensure!(
            document.version == SCHEMA_VERSION,
            "unsupported schema version {} in {}; expected {}",
            document.version,
            canonical.display(),
            SCHEMA_VERSION
        );
        if !is_root {
            ensure!(
                document.project.is_none() && document.outputs.is_none(),
                "imported fragment {} may not define project or outputs",
                canonical.display()
            );
        }

        self.add_dependency(&canonical, &bytes)?;
        self.visiting.push(canonical.clone());
        let imports = document.imports.clone();
        self.documents.push((canonical.clone(), document));

        let base = canonical
            .parent()
            .context("YAML source has no parent directory")?;
        for import in imports {
            ensure!(
                !import.path.is_absolute(),
                "import path {} in {} must be relative",
                import.path.display(),
                canonical.display()
            );
            self.visit(&base.join(&import.path), false)
                .with_context(|| {
                    format!(
                        "while resolving import {} from {}",
                        import.path.display(),
                        display_relative(&self.root, &canonical)
                    )
                })?;
        }

        self.visiting.pop();
        self.visited.insert(canonical);
        Ok(())
    }

    fn add_dependency(&mut self, path: &Path, bytes: &[u8]) -> Result<()> {
        let relative = relative_string(&self.root, path)?;
        self.dependencies
            .entry(relative.clone())
            .or_insert(Dependency {
                path: path.to_path_buf(),
                relative_path: relative,
                sha256: sha256(bytes),
            });
        Ok(())
    }

    fn finish(mut self) -> Result<Project> {
        let root_document = self
            .documents
            .iter()
            .find(|(path, _)| path == &self.config_path)
            .map(|(_, document)| document)
            .context("root document was not loaded")?;
        let project = root_document
            .project
            .as_ref()
            .context("root document must define project")?;
        ensure!(
            !project.name.trim().is_empty(),
            "project.name may not be empty"
        );
        let project_name = project.name.clone();
        let raw_outputs = root_document
            .outputs
            .as_ref()
            .context("root document must define outputs")?
            .clone();
        ensure!(
            !raw_outputs.is_empty(),
            "root document must define at least one output"
        );

        let mut outputs = BTreeMap::new();
        let mut output_paths = BTreeMap::<PathBuf, String>::new();
        for (name, raw) in &raw_outputs {
            validate_id(name, "output")?;
            ensure!(
                !raw.title.trim().is_empty(),
                "output {name} has an empty title"
            );
            let relative_path = normalize_relative(&raw.path)
                .with_context(|| format!("invalid path for output {name}"))?;
            ensure!(
                relative_path != Path::new(MANIFEST_PATH),
                "output {name} may not overwrite {MANIFEST_PATH}"
            );
            let path = self.root.join(&relative_path);
            ensure_output_location(&self.root, &path)
                .with_context(|| format!("invalid path for output {name}"))?;
            if let Some(existing) = output_paths.insert(relative_path.clone(), name.clone()) {
                bail!(
                    "outputs {existing} and {name} both write {}",
                    relative_path.display()
                );
            }
            outputs.insert(
                name.clone(),
                Output {
                    name: name.clone(),
                    path,
                    relative_path,
                    title: raw.title.clone(),
                },
            );
        }

        let documents = self.documents.clone();
        let mut sections = BTreeMap::<String, Section>::new();
        for (source, document) in &documents {
            for raw in &document.sections {
                validate_id(&raw.id, "section")?;
                ensure!(
                    !raw.title.trim().is_empty(),
                    "section {} has an empty title",
                    raw.id
                );
                if let Some(previous) = sections.get(&raw.id) {
                    bail!(
                        "duplicate section id {} in {} and {}",
                        raw.id,
                        display_relative(&self.root, &previous.source),
                        display_relative(&self.root, source)
                    );
                }
                sections.insert(
                    raw.id.clone(),
                    Section {
                        id: raw.id.clone(),
                        title: raw.title.clone(),
                        order: raw.order,
                        source: source.clone(),
                    },
                );
            }
        }
        ensure!(!sections.is_empty(), "at least one section must be defined");

        let mut rules = Vec::new();
        let mut rule_sources = BTreeMap::<String, PathBuf>::new();
        for (source, document) in &documents {
            for raw in &document.rules {
                if let Some(previous) = rule_sources.insert(raw.id.clone(), source.clone()) {
                    bail!(
                        "duplicate rule id {} in {} and {}",
                        raw.id,
                        display_relative(&self.root, &previous),
                        display_relative(&self.root, source)
                    );
                }
                let rule = self
                    .resolve_rule(source, raw, &outputs, &sections)
                    .with_context(|| {
                        format!(
                            "in rule {} from {}",
                            raw.id,
                            display_relative(&self.root, source)
                        )
                    })?;
                rules.push(rule);
            }
        }
        ensure!(!rules.is_empty(), "at least one rule must be defined");

        let dependency_paths = self
            .dependencies
            .values()
            .map(|dependency| dependency.path.clone())
            .collect::<HashSet<_>>();
        for output in outputs.values() {
            if dependency_paths.contains(&output.path) {
                bail!(
                    "output {} would overwrite source dependency {}",
                    output.name,
                    display_relative(&self.root, &output.path)
                );
            }
        }

        Ok(Project {
            root: self.root,
            config_path: self.config_path,
            name: project_name,
            outputs,
            sections,
            rules,
            dependencies: self.dependencies,
        })
    }

    fn resolve_rule(
        &mut self,
        source: &Path,
        raw: &RawRule,
        outputs: &BTreeMap<String, Output>,
        sections: &BTreeMap<String, Section>,
    ) -> Result<Rule> {
        validate_id(&raw.id, "rule")?;
        if let Some(slot) = &raw.slot {
            validate_id(slot, "slot")?;
        }
        ensure!(
            !raw.targets.is_empty(),
            "rule {} must have at least one target",
            raw.id
        );
        let mut seen_targets = HashSet::new();
        for target in &raw.targets {
            ensure!(
                seen_targets.insert(target),
                "rule {} contains duplicate target {}",
                raw.id,
                target
            );
            if target != "*" {
                validate_id(target, "target")?;
                ensure!(
                    outputs.contains_key(target),
                    "rule {} references unknown target {}",
                    raw.id,
                    target
                );
            }
        }
        ensure!(
            sections.contains_key(&raw.section),
            "rule {} references unknown section {}",
            raw.id,
            raw.section
        );

        let source_directory = source
            .parent()
            .context("rule source has no parent directory")?;
        let (content, content_source) = match (&raw.content.inline, &raw.content.file) {
            (Some(inline), None) => (normalize_markdown(inline), None),
            (None, Some(file)) => {
                ensure!(
                    !file.is_absolute(),
                    "content file for rule {} must be relative",
                    raw.id
                );
                ensure_markdown_extension(file, &raw.id)?;
                let canonical = source_directory
                    .join(file)
                    .canonicalize()
                    .with_context(|| {
                        format!(
                            "failed to resolve content file {} for rule {}",
                            file.display(),
                            raw.id
                        )
                    })?;
                ensure_inside(&self.root, &canonical, "content file")?;
                let bytes = fs::read(&canonical).with_context(|| {
                    format!("failed to read content file {}", canonical.display())
                })?;
                let text = String::from_utf8(bytes.clone()).map_err(|_| {
                    anyhow!("content file {} is not valid UTF-8", canonical.display())
                })?;
                self.add_dependency(&canonical, &bytes)?;
                (normalize_markdown(&text), Some(canonical))
            }
            (Some(_), Some(_)) => bail!(
                "rule {} content must define only one of inline or file",
                raw.id
            ),
            (None, None) => bail!("rule {} content must define one of inline or file", raw.id),
        };
        ensure!(
            !content.trim().is_empty(),
            "rule {} has empty content",
            raw.id
        );
        let checks = raw
            .checks
            .iter()
            .map(resolve_check)
            .collect::<Result<Vec<_>>>()?;

        Ok(Rule {
            id: raw.id.clone(),
            slot: raw.slot.clone(),
            priority: raw.priority,
            targets: raw.targets.clone(),
            section: raw.section.clone(),
            order: raw.order,
            content,
            source: source.to_path_buf(),
            content_source,
            checks,
        })
    }
}

fn resolve_check(raw: &RawCheck) -> Result<Check> {
    match raw {
        RawCheck::PackageScript { manifest, script } => {
            let manifest = normalize_relative(manifest)
                .with_context(|| "invalid package-script manifest path")?;
            ensure!(
                !script.trim().is_empty(),
                "package-script check script may not be empty"
            );
            Ok(Check::PackageScript {
                manifest,
                script: script.clone(),
            })
        }
        RawCheck::PathExists { path, kind } => Ok(Check::PathExists {
            path: normalize_relative(path).with_context(|| "invalid path-exists path")?,
            kind: match kind {
                RawPathKind::Any => PathKind::Any,
                RawPathKind::File => PathKind::File,
                RawPathKind::Directory => PathKind::Directory,
            },
        }),
    }
}

pub fn compile_project(project: &Project) -> Result<CompiledProject> {
    let source_fingerprint = source_fingerprint(project);
    let mut compiled_outputs = BTreeMap::new();
    let mut effective_checks = BTreeMap::<(&str, usize), Vec<String>>::new();

    for (target, output) in &project.outputs {
        let applicable = project
            .rules
            .iter()
            .filter(|rule| {
                rule.targets
                    .iter()
                    .any(|item| item == "*" || item == target)
            })
            .collect::<Vec<_>>();
        let mut selected = applicable
            .iter()
            .copied()
            .filter(|rule| rule.slot.is_none())
            .collect::<Vec<_>>();
        let mut slots = BTreeMap::<&str, Vec<&Rule>>::new();
        for rule in applicable
            .iter()
            .copied()
            .filter(|rule| rule.slot.is_some())
        {
            slots
                .entry(rule.slot.as_deref().unwrap())
                .or_default()
                .push(rule);
        }

        let mut suppressed = Vec::new();
        for (slot, mut candidates) in slots {
            candidates.sort_by(|left, right| {
                right
                    .priority
                    .cmp(&left.priority)
                    .then_with(|| left.id.cmp(&right.id))
            });
            let winner = candidates[0];
            if candidates.len() > 1 && candidates[1].priority == winner.priority {
                let tied = candidates
                    .iter()
                    .take_while(|candidate| candidate.priority == winner.priority)
                    .map(|candidate| {
                        format!(
                            "{} ({})",
                            candidate.id,
                            display_relative(&project.root, &candidate.source)
                        )
                    })
                    .collect::<Vec<_>>();
                bail!(
                    "target {target} has equal-priority rules in slot {slot}: {} (priority {})",
                    tied.join(", "),
                    winner.priority
                );
            }
            selected.push(winner);
            for loser in candidates.into_iter().skip(1) {
                suppressed.push(Suppression {
                    rule: loser.id.clone(),
                    winner: winner.id.clone(),
                    slot: slot.to_owned(),
                });
            }
        }

        selected.sort_by(|left, right| {
            let left_section = &project.sections[&left.section];
            let right_section = &project.sections[&right.section];
            left_section
                .order
                .cmp(&right_section.order)
                .then_with(|| left.order.cmp(&right.order))
                .then_with(|| left.id.cmp(&right.id))
        });
        suppressed.sort_by(|left, right| {
            left.slot
                .cmp(&right.slot)
                .then_with(|| left.rule.cmp(&right.rule))
        });

        for rule in &selected {
            for index in 0..rule.checks.len() {
                effective_checks
                    .entry((&rule.id, index))
                    .or_default()
                    .push(target.clone());
            }
        }

        let content = render_output(project, output, &selected, &source_fingerprint);
        let applied = selected
            .iter()
            .map(|rule| rule.id.clone())
            .collect::<Vec<_>>();
        compiled_outputs.insert(
            target.clone(),
            CompiledOutput {
                name: target.clone(),
                path: output.path.clone(),
                relative_path: output.relative_path.clone(),
                sha256: sha256(content.as_bytes()),
                content,
                applied,
                suppressed,
            },
        );
    }

    validate_effective_checks(project, &effective_checks)?;

    let dependencies = project
        .dependencies
        .values()
        .map(|dependency| ManifestDependency {
            path: dependency.relative_path.clone(),
            sha256: dependency.sha256.clone(),
        })
        .collect();
    let outputs = compiled_outputs
        .iter()
        .map(|(name, output)| {
            (
                name.clone(),
                ManifestOutput {
                    path: path_string(&output.relative_path),
                    sha256: output.sha256.clone(),
                    rules: output.applied.clone(),
                    suppressed: output
                        .suppressed
                        .iter()
                        .map(|item| ManifestSuppressed {
                            rule: item.rule.clone(),
                            winner: item.winner.clone(),
                            slot: item.slot.clone(),
                        })
                        .collect(),
                },
            )
        })
        .collect();
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        schema_version: SCHEMA_VERSION,
        compiler_version: env!("CARGO_PKG_VERSION").to_owned(),
        root_config: relative_string(&project.root, &project.config_path)?,
        source_fingerprint: source_fingerprint.clone(),
        dependencies,
        outputs,
    };

    Ok(CompiledProject {
        source_fingerprint,
        outputs: compiled_outputs,
        manifest,
    })
}

fn validate_effective_checks(
    project: &Project,
    effective_checks: &BTreeMap<(&str, usize), Vec<String>>,
) -> Result<()> {
    let mut failures = Vec::new();
    for ((rule_id, check_index), targets) in effective_checks {
        let rule = project
            .rules
            .iter()
            .find(|rule| rule.id == *rule_id)
            .expect("effective check must reference a loaded rule");
        let check = &rule.checks[*check_index];
        if let Err(reason) = validate_check(&project.root, check) {
            failures.push(format!(
                "rule {} ({}), targets [{}], {}: {}",
                rule.id,
                display_relative(&project.root, &rule.source),
                targets.join(", "),
                describe_check(check),
                reason
            ));
        }
    }
    if failures.is_empty() {
        return Ok(());
    }

    let mut message = String::from("guardrail validation failed:");
    for failure in failures {
        write!(message, "\n  - {failure}")?;
    }
    bail!(message)
}

fn validate_check(root: &Path, check: &Check) -> std::result::Result<(), String> {
    match check {
        Check::PackageScript { manifest, script } => {
            let canonical = canonicalize_checked_path(root, manifest, "package manifest")?;
            if !canonical.is_file() {
                return Err(format!(
                    "package manifest {} is not a file",
                    path_string(manifest)
                ));
            }
            let text = fs::read_to_string(&canonical).map_err(|error| {
                format!(
                    "failed to read package manifest {}: {error}",
                    path_string(manifest)
                )
            })?;
            let package: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
                format!(
                    "failed to parse package manifest {} as JSON: {error}",
                    path_string(manifest)
                )
            })?;
            let scripts = package
                .get("scripts")
                .and_then(serde_json::Value::as_object)
                .ok_or_else(|| {
                    format!(
                        "package manifest {} does not define a scripts object",
                        path_string(manifest)
                    )
                })?;
            let Some(command) = scripts.get(script) else {
                return Err(format!(
                    "script {script:?} does not exist in package manifest {}",
                    path_string(manifest)
                ));
            };
            if !command.is_string() {
                return Err(format!(
                    "script {script:?} in package manifest {} must be a string",
                    path_string(manifest)
                ));
            }
            Ok(())
        }
        Check::PathExists { path, kind } => {
            let canonical = canonicalize_checked_path(root, path, "required path")?;
            let valid_kind = match kind {
                PathKind::Any => true,
                PathKind::File => canonical.is_file(),
                PathKind::Directory => canonical.is_dir(),
            };
            if valid_kind {
                Ok(())
            } else {
                Err(format!(
                    "required path {} is not a {}",
                    path_string(path),
                    path_kind_name(*kind)
                ))
            }
        }
    }
}

fn canonicalize_checked_path(
    root: &Path,
    relative: &Path,
    kind: &str,
) -> std::result::Result<PathBuf, String> {
    let path = root.join(relative);
    let canonical = path.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("{kind} {} does not exist", path_string(relative))
        } else {
            format!(
                "failed to resolve {kind} {}: {error}",
                path_string(relative)
            )
        }
    })?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "{kind} {} resolves outside the project root",
            path_string(relative)
        ));
    }
    Ok(canonical)
}

fn describe_check(check: &Check) -> String {
    match check {
        Check::PackageScript { manifest, script } => format!(
            "package-script check ({}#scripts.{script})",
            path_string(manifest)
        ),
        Check::PathExists { path, kind } => format!(
            "path-exists check ({}; kind {})",
            path_string(path),
            path_kind_name(*kind)
        ),
    }
}

fn path_kind_name(kind: PathKind) -> &'static str {
    match kind {
        PathKind::Any => "any",
        PathKind::File => "file",
        PathKind::Directory => "directory",
    }
}

fn render_output(project: &Project, output: &Output, rules: &[&Rule], fingerprint: &str) -> String {
    let config = display_relative(&project.root, &project.config_path);
    let mut rendered = format!(
        "<!-- Generated by ctcx from {config}. DO NOT EDIT. Fingerprint: {fingerprint} -->\n\n# {}\n",
        output.title.trim()
    );
    let mut current_section: Option<&str> = None;
    for rule in rules {
        if current_section != Some(rule.section.as_str()) {
            let section = &project.sections[&rule.section];
            write!(rendered, "\n## {}\n", section.title.trim()).unwrap();
            current_section = Some(&rule.section);
        }
        write!(rendered, "\n{}\n", rule.content.trim_end()).unwrap();
    }
    rendered
}

pub fn build_project(
    project: &Project,
    compiled: &CompiledProject,
    safety: BuildSafety,
    dry_run: bool,
) -> Result<Vec<PathBuf>> {
    let manifest_path = project.root.join(MANIFEST_PATH);
    ensure_output_location(&project.root, &manifest_path)?;
    let force = safety == BuildSafety::Force;
    let previous = match read_manifest(&manifest_path) {
        Ok(manifest) => manifest,
        Err(_) if force => None,
        Err(error) => return Err(error),
    };
    if !force
        && previous.as_ref().is_some_and(|manifest| {
            manifest.source_fingerprint == compiled.source_fingerprint
                && manifest != &compiled.manifest
        })
    {
        bail!("refusing to build with a modified manifest; use --force to regenerate it");
    }
    let configured_paths = compiled
        .outputs
        .values()
        .map(|output| path_string(&output.relative_path))
        .collect::<HashSet<_>>();

    let mut obsolete = Vec::new();
    if let Some(previous) = &previous {
        for old in previous.outputs.values() {
            if !configured_paths.contains(&old.path) {
                let relative = normalize_relative(Path::new(&old.path))
                    .context("previous manifest contains an invalid output path")?;
                let path = project.root.join(relative);
                ensure_output_location(&project.root, &path)?;
                if path.exists() {
                    let actual = hash_file(&path)?;
                    if !force && actual != old.sha256 {
                        bail!(
                            "refusing to remove modified obsolete output {}; use --force to override",
                            path.display()
                        );
                    }
                    obsolete.push(path);
                }
            }
        }
    }

    for (name, output) in &compiled.outputs {
        if !output.path.exists() {
            continue;
        }
        let actual = hash_file(&output.path)?;
        let tracked = previous
            .as_ref()
            .and_then(|manifest| manifest.outputs.get(name))
            .filter(|entry| entry.path == path_string(&output.relative_path));
        match tracked {
            Some(entry) if actual == entry.sha256 => {}
            Some(_) if !force => bail!(
                "refusing to overwrite modified output {}; use --force to override",
                output.path.display()
            ),
            None if !force => bail!(
                "refusing to overwrite untracked output {}; use --force to adopt it",
                output.path.display()
            ),
            _ => {}
        }
    }

    let manifest_text = serialize_manifest(&compiled.manifest)?;
    let mut changed = Vec::new();
    changed.extend(obsolete.iter().cloned());
    for output in compiled.outputs.values() {
        let current = fs::read(&output.path).ok();
        if current.as_deref() != Some(output.content.as_bytes()) {
            changed.push(output.path.clone());
        }
    }
    let current_manifest = fs::read(&manifest_path).ok();
    if current_manifest.as_deref() != Some(manifest_text.as_bytes()) {
        changed.push(manifest_path.clone());
    }

    if dry_run || changed.is_empty() {
        return Ok(changed);
    }

    for path in obsolete {
        fs::remove_file(&path)
            .with_context(|| format!("failed to remove obsolete output {}", path.display()))?;
    }
    for output in compiled.outputs.values() {
        if fs::read(&output.path).ok().as_deref() != Some(output.content.as_bytes()) {
            atomic_write(&output.path, output.content.as_bytes())?;
        }
    }
    atomic_write(&manifest_path, manifest_text.as_bytes())?;
    Ok(changed)
}

pub fn check_project(
    project: &Project,
    compiled: &CompiledProject,
    target: Option<&str>,
) -> Result<DriftReport> {
    validate_target(compiled, target)?;
    let manifest_path = project.root.join(MANIFEST_PATH);
    let mut report = DriftReport::default();
    let manifest = match read_manifest(&manifest_path) {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            report.push(format!("missing manifest {MANIFEST_PATH}"));
            for (name, output) in selected_outputs(compiled, target) {
                if !output.path.exists() {
                    report.push(format!(
                        "missing output {name} ({})",
                        output.relative_path.display()
                    ));
                } else {
                    report.push(format!(
                        "untracked output {name} ({})",
                        output.relative_path.display()
                    ));
                }
            }
            return Ok(report);
        }
        Err(error) => {
            report.push(format!("manifest mismatch: {error:#}"));
            return Ok(report);
        }
    };

    let metadata_mismatch = manifest.version != MANIFEST_VERSION
        || manifest.schema_version != SCHEMA_VERSION
        || manifest.compiler_version != env!("CARGO_PKG_VERSION")
        || manifest.root_config != relative_string(&project.root, &project.config_path)?;
    if metadata_mismatch {
        report.push("manifest metadata does not match this project or compiler");
    }
    let sources_changed = manifest.source_fingerprint != compiled.source_fingerprint;
    if !sources_changed
        && !metadata_mismatch
        && (manifest.dependencies != compiled.manifest.dependencies
            || manifest.outputs != compiled.manifest.outputs)
    {
        report.push("manifest contents do not match the expected compilation");
    }

    for (name, output) in selected_outputs(compiled, target) {
        let Some(recorded) = manifest.outputs.get(name) else {
            report.push(format!("manifest is missing output {name}"));
            continue;
        };
        if recorded.path != path_string(&output.relative_path) {
            report.push(format!("manifest path mismatch for output {name}"));
            continue;
        }
        if !output.path.exists() {
            report.push(format!(
                "missing output {name} ({})",
                output.relative_path.display()
            ));
            continue;
        }
        let actual = hash_file(&output.path)?;
        let modified = actual != recorded.sha256;
        let differs_from_expected = actual != output.sha256;
        match (sources_changed, modified, differs_from_expected) {
            (true, true, _) => report.push(format!(
                "stale and modified output {name} ({})",
                output.relative_path.display()
            )),
            (true, false, _) => report.push(format!(
                "stale output {name} ({})",
                output.relative_path.display()
            )),
            (false, true, _) => report.push(format!(
                "modified output {name} ({})",
                output.relative_path.display()
            )),
            (false, false, true) => report.push(format!("manifest mismatch for output {name}")),
            _ => {}
        }
    }

    if target.is_none() {
        for (name, old) in &manifest.outputs {
            if !compiled.outputs.contains_key(name) {
                report.push(format!("obsolete output {name} ({})", old.path));
            }
        }
    }
    Ok(report)
}

pub fn render_diffs(
    project: &Project,
    compiled: &CompiledProject,
    target: Option<&str>,
) -> Result<String> {
    validate_target(compiled, target)?;
    let mut rendered = String::new();
    for (_, output) in selected_outputs(compiled, target) {
        let current = match fs::read_to_string(&output.path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", output.path.display()));
            }
        };
        if current != output.content {
            let old_name = format!("a/{}", output.relative_path.display());
            let new_name = format!("b/{}", output.relative_path.display());
            let diff = TextDiff::from_lines(&current, &output.content)
                .unified_diff()
                .header(&old_name, &new_name)
                .to_string();
            rendered.push_str(&diff);
        }
    }

    if target.is_none()
        && let Some(manifest) = read_manifest(&project.root.join(MANIFEST_PATH))?
    {
        for (name, old) in manifest.outputs {
            if compiled.outputs.contains_key(&name) {
                continue;
            }
            let relative = normalize_relative(Path::new(&old.path))?;
            let path = project.root.join(&relative);
            if let Ok(current) = fs::read_to_string(&path) {
                let old_name = format!("a/{}", relative.display());
                let new_name = "/dev/null";
                let empty = String::new();
                rendered.push_str(
                    &TextDiff::from_lines(&current, &empty)
                        .unified_diff()
                        .header(&old_name, new_name)
                        .to_string(),
                );
            }
        }
    }
    Ok(rendered)
}

pub fn explain_target(
    project: &Project,
    compiled: &CompiledProject,
    target: &str,
    slot: Option<&str>,
) -> Result<String> {
    let output = compiled
        .outputs
        .get(target)
        .with_context(|| format!("unknown target {target}"))?;
    if let Some(slot) = slot {
        validate_id(slot, "slot")?;
    }
    let mut rendered = format!("target {target} -> {}\n", output.relative_path.display());
    let rule_map = project
        .rules
        .iter()
        .map(|rule| (rule.id.as_str(), rule))
        .collect::<BTreeMap<_, _>>();

    for id in &output.applied {
        let rule = rule_map[id.as_str()];
        if slot.is_none() || rule.slot.as_deref() == slot {
            writeln!(
                rendered,
                "  applied {} [section={}, slot={}, priority={}, source={}]",
                rule.id,
                rule.section,
                rule.slot.as_deref().unwrap_or("-"),
                rule.priority,
                display_relative(&project.root, &rule.source)
            )?;
        }
    }
    for item in &output.suppressed {
        if slot.is_none() || Some(item.slot.as_str()) == slot {
            writeln!(
                rendered,
                "  suppressed {} by {} [slot={}]",
                item.rule, item.winner, item.slot
            )?;
        }
    }
    Ok(rendered)
}

pub fn explain_rule(
    project: &Project,
    compiled: &CompiledProject,
    rule_id: &str,
) -> Result<String> {
    let rule = project
        .rules
        .iter()
        .find(|rule| rule.id == rule_id)
        .with_context(|| format!("unknown rule {rule_id}"))?;
    let mut rendered = format!(
        "rule {}\n  source: {}\n  content: {}\n  section: {}\n  slot: {}\n  priority: {}\n  targets: {}\n",
        rule.id,
        display_relative(&project.root, &rule.source),
        rule.content_source
            .as_ref()
            .map(|path| display_relative(&project.root, path))
            .unwrap_or_else(|| "inline".to_owned()),
        rule.section,
        rule.slot.as_deref().unwrap_or("-"),
        rule.priority,
        rule.targets.join(", ")
    );
    for (target, output) in &compiled.outputs {
        let status = rule_status(output, rule_id);
        match status {
            ExplainStatus::Applied => writeln!(rendered, "  {target}: applied")?,
            ExplainStatus::Suppressed { winner } => {
                writeln!(rendered, "  {target}: suppressed by {winner}")?
            }
            ExplainStatus::NotTargeted => writeln!(rendered, "  {target}: not targeted")?,
        }
    }
    Ok(rendered)
}

fn rule_status(output: &CompiledOutput, rule_id: &str) -> ExplainStatus {
    if output.applied.iter().any(|id| id == rule_id) {
        ExplainStatus::Applied
    } else if let Some(item) = output.suppressed.iter().find(|item| item.rule == rule_id) {
        ExplainStatus::Suppressed {
            winner: item.winner.clone(),
        }
    } else {
        ExplainStatus::NotTargeted
    }
}

pub fn init_project(config_path: &Path, force: bool) -> Result<Vec<PathBuf>> {
    ensure_yaml_extension(config_path, "configuration")?;
    let config_path = if config_path.is_absolute() {
        config_path.to_path_buf()
    } else {
        std::env::current_dir()?.join(config_path)
    };
    let root = config_path
        .parent()
        .context("configuration path has no parent")?;
    fs::create_dir_all(root).with_context(|| format!("failed to create {}", root.display()))?;
    let root = root
        .canonicalize()
        .context("failed to resolve project root")?;
    let config_path = root.join(
        config_path
            .file_name()
            .context("configuration path has no filename")?,
    );
    let instruction_path = root.join("instructions/project.md");
    let initial_paths = [
        config_path.clone(),
        instruction_path.clone(),
        root.join("AGENTS.md"),
        root.join("CLAUDE.md"),
        root.join(MANIFEST_PATH),
    ];
    if !force {
        let existing = initial_paths
            .iter()
            .filter(|path| path.exists())
            .collect::<Vec<_>>();
        if !existing.is_empty() {
            bail!(
                "refusing to initialize over existing files: {}; use --force to override",
                existing
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    let project_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("project");
    let project_scalar = serde_yaml_ng::to_string(project_name)?.trim().to_owned();
    let config = format!(
        "version: 1\n\nproject:\n  name: {project_scalar}\n\noutputs:\n  agents:\n    path: AGENTS.md\n    title: Project Agent Instructions\n  claude:\n    path: CLAUDE.md\n    title: Claude Code Instructions\n\nsections:\n  - id: workflow\n    title: Workflow\n    order: 100\n\nrules:\n  - id: project-workflow\n    priority: 0\n    targets: [\"*\"]\n    section: workflow\n    order: 100\n    content:\n      file: instructions/project.md\n"
    );
    let instruction =
        "Describe the commands, constraints, and workflow agents must follow in this project.\n";
    atomic_write(&config_path, config.as_bytes())?;
    atomic_write(&instruction_path, instruction.as_bytes())?;

    let project = load_project(&config_path)?;
    let compiled = compile_project(&project)?;
    let mut written = vec![config_path, instruction_path];
    written.extend(build_project(
        &project,
        &compiled,
        BuildSafety::Force,
        false,
    )?);
    written.sort();
    written.dedup();
    Ok(written)
}

fn parse_yaml_document(path: &Path, text: &str) -> Result<RawDocument> {
    let mut documents = serde_yaml_ng::Deserializer::from_str(text);
    let first = documents.next().context("YAML source is empty")?;
    let document = RawDocument::deserialize(first)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    ensure!(
        documents.next().is_none(),
        "YAML source {} contains multiple documents; only one is allowed",
        path.display()
    );
    Ok(document)
}

fn serialize_manifest(manifest: &Manifest) -> Result<String> {
    let mut text = String::from("# Generated by ctcx. DO NOT EDIT.\n");
    text.push_str(&serde_yaml_ng::to_string(manifest).context("failed to serialize manifest")?);
    Ok(text)
}

fn read_manifest(path: &Path) -> Result<Option<Manifest>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read manifest {}", path.display()));
        }
    };
    let manifest = serde_yaml_ng::from_str(&text)
        .with_context(|| format!("failed to parse manifest {}", path.display()))?;
    Ok(Some(manifest))
}

fn source_fingerprint(project: &Project) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!(
        "schema={SCHEMA_VERSION}\ncompiler={}\n",
        env!("CARGO_PKG_VERSION")
    ));
    for dependency in project.dependencies.values() {
        hasher.update(dependency.relative_path.as_bytes());
        hasher.update([0]);
        hasher.update(dependency.sha256.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hash_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(sha256(&bytes))
}

fn normalize_markdown(content: &str) -> String {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end()
        .to_owned()
}

fn ensure_yaml_extension(path: &Path, kind: &str) -> Result<()> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    ensure!(
        extension.eq_ignore_ascii_case("yaml") || extension.eq_ignore_ascii_case("yml"),
        "{kind} {} must be a YAML file (.yaml or .yml)",
        path.display()
    );
    Ok(())
}

fn ensure_markdown_extension(path: &Path, rule: &str) -> Result<()> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    ensure!(
        extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown"),
        "content file {} for rule {} must be Markdown (.md or .markdown)",
        path.display(),
        rule
    );
    Ok(())
}

fn validate_id(id: &str, kind: &str) -> Result<()> {
    let mut characters = id.chars();
    let first = characters
        .next()
        .with_context(|| format!("{kind} identifier may not be empty"))?;
    ensure!(
        first.is_ascii_lowercase() || first.is_ascii_digit(),
        "invalid {kind} identifier {id:?}: it must begin with a lowercase letter or digit"
    );
    ensure!(
        characters.all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        }),
        "invalid {kind} identifier {id:?}: use lowercase letters, digits, '.', '_', or '-'"
    );
    Ok(())
}

fn normalize_relative(path: &Path) -> Result<PathBuf> {
    ensure!(!path.as_os_str().is_empty(), "path may not be empty");
    ensure!(
        !path.is_absolute(),
        "path {} must be relative",
        path.display()
    );
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => {
                ensure!(
                    normalized.pop(),
                    "path {} escapes the project root",
                    path.display()
                );
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("path {} must be relative", path.display())
            }
        }
    }
    ensure!(
        !normalized.as_os_str().is_empty(),
        "path {} resolves to the project root",
        path.display()
    );
    Ok(normalized)
}

fn ensure_output_location(root: &Path, path: &Path) -> Result<()> {
    let mut ancestor = path;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .context("output path has no existing ancestor")?;
    }
    let canonical = ancestor
        .canonicalize()
        .with_context(|| format!("failed to resolve output ancestor {}", ancestor.display()))?;
    ensure_inside(root, &canonical, "output")
}

fn ensure_inside(root: &Path, path: &Path, kind: &str) -> Result<()> {
    ensure!(
        path.starts_with(root),
        "{kind} path {} escapes project root {}",
        path.display(),
        root.display()
    );
    Ok(())
}

fn relative_string(root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(root).with_context(|| {
        format!(
            "{} is outside project root {}",
            path.display(),
            root.display()
        )
    })?;
    Ok(path_string(relative))
}

fn display_relative(root: &Path, path: &Path) -> String {
    relative_string(root, path).unwrap_or_else(|_| path.display().to_string())
}

fn path_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn selected_outputs<'a>(
    compiled: &'a CompiledProject,
    target: Option<&str>,
) -> Vec<(&'a String, &'a CompiledOutput)> {
    compiled
        .outputs
        .iter()
        .filter(|(name, _)| target.is_none_or(|target| name.as_str() == target))
        .collect()
}

fn validate_target(compiled: &CompiledProject, target: Option<&str>) -> Result<()> {
    if let Some(target) = target {
        ensure!(
            compiled.outputs.contains_key(target),
            "unknown target {target}"
        );
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("destination has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    temporary
        .write_all(bytes)
        .with_context(|| format!("failed to stage {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}
