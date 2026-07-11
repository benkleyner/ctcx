use crate::model::{
    MANIFEST_VERSION, Manifest, ManifestDependency, ManifestOutput, ManifestSuppressed, RawCheck,
    RawCondition, RawDocument, RawHeaderMode, RawOutputFormat, RawPathKind, RawRule, RawTemplate,
    SCHEMA_VERSION,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value;
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
    pub documents: Vec<SourceDocument>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceLocation {
    pub path: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone)]
pub struct SourceDocument {
    pub path: PathBuf,
    pub imports: Vec<SourceImport>,
    pub location: SourceLocation,
}

#[derive(Debug, Clone)]
pub struct SourceImport {
    pub path: PathBuf,
    pub location: SourceLocation,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub title: String,
    pub format: OutputFormat,
    pub front_matter: BTreeMap<String, Value>,
    pub header: Header,
    pub templates: OutputTemplates,
    pub location: SourceLocation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Markdown,
    Agents,
    Claude,
    Cursor,
    Copilot,
    Windsurf,
    Cline,
    Template,
}

impl OutputFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Agents => "agents",
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Copilot => "copilot",
            Self::Windsurf => "windsurf",
            Self::Cline => "cline",
            Self::Template => "template",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Header {
    Default,
    Omit,
    Template(Template),
}

#[derive(Debug, Default, Clone)]
pub struct OutputTemplates {
    pub output: Option<Template>,
    pub section: Option<Template>,
}

#[derive(Debug, Clone)]
pub struct Template {
    pub content: String,
    pub source: Option<PathBuf>,
    pub location: SourceLocation,
    pub source_locations: Vec<SourceLocation>,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub order: i32,
    pub source: PathBuf,
    pub location: SourceLocation,
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
    pub location: SourceLocation,
    pub content_location: SourceLocation,
    pub when: Option<Condition>,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
    PathExists { path: PathBuf, kind: PathKind },
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
    pub inapplicable: Vec<String>,
    pub provenance: Vec<ProvenanceRange>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProvenanceRange {
    pub start_line: usize,
    pub end_line: usize,
    pub kind: ProvenanceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    pub sources: Vec<SourceLocation>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceKind {
    Output,
    RuleContent,
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
    Inapplicable,
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
            let format = output_format(raw.format);
            validate_output_format(name, format, &relative_path, &raw.front_matter)?;
            let templates = OutputTemplates {
                output: raw
                    .templates
                    .output
                    .as_ref()
                    .map(|template| self.resolve_template(template, "output template", name))
                    .transpose()?,
                section: raw
                    .templates
                    .section
                    .as_ref()
                    .map(|template| self.resolve_template(template, "section template", name))
                    .transpose()?,
            };
            ensure!(
                format != OutputFormat::Template || templates.output.is_some(),
                "template output {name} must define templates.output"
            );
            ensure!(
                format != OutputFormat::Template || templates.section.is_some(),
                "template output {name} must define templates.section"
            );
            let header = match raw.header.as_ref() {
                None => Header::Default,
                Some(header) => match header.mode {
                    RawHeaderMode::Default => {
                        ensure!(
                            header.template.is_none(),
                            "output {name} header.template is only valid when header.mode is template"
                        );
                        Header::Default
                    }
                    RawHeaderMode::Omit => {
                        ensure!(
                            header.template.is_none(),
                            "output {name} header.template is only valid when header.mode is template"
                        );
                        Header::Omit
                    }
                    RawHeaderMode::Template => Header::Template(self.resolve_template(
                        header.template.as_ref().context(format!(
                            "output {name} header.mode template requires header.template"
                        ))?,
                        "header template",
                        name,
                    )?),
                },
            };
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
                    format,
                    front_matter: raw.front_matter.clone(),
                    header,
                    templates,
                    location: locate_mapping_key(&self.root, &self.config_path, "outputs", name),
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
                        location: locate_id(&self.root, source, "sections", &raw.id),
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

        let source_documents = documents
            .iter()
            .map(|(path, document)| SourceDocument {
                path: path.clone(),
                imports: document
                    .imports
                    .iter()
                    .map(|import| SourceImport {
                        path: path.parent().unwrap().join(&import.path),
                        location: locate_import(&self.root, path, &import.path),
                    })
                    .collect(),
                location: SourceLocation {
                    path: display_relative(&self.root, path),
                    line: 1,
                    column: 1,
                },
            })
            .collect();
        Ok(Project {
            root: self.root,
            config_path: self.config_path,
            name: project_name,
            outputs,
            sections,
            rules,
            dependencies: self.dependencies,
            documents: source_documents,
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
        let when = raw.when.as_ref().map(resolve_condition).transpose()?;

        let content_location = content_source
            .as_ref()
            .map(|path| SourceLocation {
                path: display_relative(&self.root, path),
                line: 1,
                column: 1,
            })
            .unwrap_or_else(|| locate_rule_content(&self.root, source, &raw.id));
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
            location: locate_id(&self.root, source, "rules", &raw.id),
            content_location,
            when,
            checks,
        })
    }

    fn resolve_template(
        &mut self,
        raw: &RawTemplate,
        kind: &str,
        output: &str,
    ) -> Result<Template> {
        let (content, source) = match (&raw.inline, &raw.file) {
            (Some(inline), None) => (normalize_template(inline), None),
            (None, Some(file)) => {
                ensure!(
                    !file.is_absolute(),
                    "{kind} file for output {output} must be relative"
                );
                let source_directory = self
                    .config_path
                    .parent()
                    .context("configuration path has no parent directory")?;
                let canonical = source_directory
                    .join(file)
                    .canonicalize()
                    .with_context(|| {
                        format!(
                            "failed to resolve {kind} file {} for output {output}",
                            file.display()
                        )
                    })?;
                ensure_inside(&self.root, &canonical, kind)?;
                let bytes = fs::read(&canonical).with_context(|| {
                    format!("failed to read {kind} file {}", canonical.display())
                })?;
                let text = String::from_utf8(bytes.clone()).map_err(|_| {
                    anyhow!("{kind} file {} is not valid UTF-8", canonical.display())
                })?;
                self.add_dependency(&canonical, &bytes)?;
                (normalize_template(&text), Some(canonical))
            }
            (Some(_), Some(_)) => {
                bail!("{kind} for output {output} must define only one of inline or file")
            }
            (None, None) => bail!("{kind} for output {output} must define one of inline or file"),
        };
        ensure!(
            !content.trim().is_empty(),
            "{kind} for output {output} may not be empty"
        );
        let location = source
            .as_ref()
            .map(|path| SourceLocation {
                path: display_relative(&self.root, path),
                line: 1,
                column: 1,
            })
            .unwrap_or_else(|| SourceLocation {
                ..locate_output_template(&self.root, &self.config_path, output, raw)
            });
        let source_locations = source
            .as_ref()
            .map(|path| {
                content
                    .lines()
                    .enumerate()
                    .map(|(index, line)| SourceLocation {
                        path: display_relative(&self.root, path),
                        line: index + 1,
                        column: line.len() - line.trim_start().len() + 1,
                    })
                    .collect()
            })
            .unwrap_or_else(|| vec![location.clone()]);
        Ok(Template {
            content,
            source,
            location,
            source_locations,
        })
    }
}

fn resolve_condition(raw: &RawCondition) -> Result<Condition> {
    match raw {
        RawCondition::All { conditions } => {
            ensure!(
                !conditions.is_empty(),
                "all condition must contain at least one condition"
            );
            Ok(Condition::All(
                conditions
                    .iter()
                    .map(resolve_condition)
                    .collect::<Result<Vec<_>>>()?,
            ))
        }
        RawCondition::Any { conditions } => {
            ensure!(
                !conditions.is_empty(),
                "any condition must contain at least one condition"
            );
            Ok(Condition::Any(
                conditions
                    .iter()
                    .map(resolve_condition)
                    .collect::<Result<Vec<_>>>()?,
            ))
        }
        RawCondition::Not { condition } => {
            Ok(Condition::Not(Box::new(resolve_condition(condition)?)))
        }
        RawCondition::PathExists { path, kind } => Ok(Condition::PathExists {
            path: normalize_relative(path).with_context(|| "invalid path-exists condition path")?,
            kind: match kind {
                RawPathKind::Any => PathKind::Any,
                RawPathKind::File => PathKind::File,
                RawPathKind::Directory => PathKind::Directory,
            },
        }),
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

fn output_format(raw: RawOutputFormat) -> OutputFormat {
    match raw {
        RawOutputFormat::Markdown => OutputFormat::Markdown,
        RawOutputFormat::Agents => OutputFormat::Agents,
        RawOutputFormat::Claude => OutputFormat::Claude,
        RawOutputFormat::Cursor => OutputFormat::Cursor,
        RawOutputFormat::Copilot => OutputFormat::Copilot,
        RawOutputFormat::Windsurf => OutputFormat::Windsurf,
        RawOutputFormat::Cline => OutputFormat::Cline,
        RawOutputFormat::Template => OutputFormat::Template,
    }
}

fn validate_output_format(
    name: &str,
    format: OutputFormat,
    path: &Path,
    front_matter: &BTreeMap<String, Value>,
) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let has_prefix = |prefix: &str| path.starts_with(Path::new(prefix));
    let has_extension = |extension: &str| {
        path.extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(extension))
    };

    match format {
        OutputFormat::Markdown | OutputFormat::Template => {}
        OutputFormat::Agents => ensure!(
            file_name == "AGENTS.md",
            "agents output {name} must write an AGENTS.md file"
        ),
        OutputFormat::Claude => ensure!(
            file_name == "CLAUDE.md",
            "claude output {name} must write a CLAUDE.md file"
        ),
        OutputFormat::Cursor => ensure!(
            has_prefix(".cursor/rules") && path.components().count() > 2 && has_extension("mdc"),
            "cursor output {name} must write .cursor/rules/**/*.mdc"
        ),
        OutputFormat::Copilot => ensure!(
            path == Path::new(".github/copilot-instructions.md")
                || (has_prefix(".github/instructions")
                    && path.components().count() > 2
                    && file_name.ends_with(".instructions.md")),
            "copilot output {name} must write .github/copilot-instructions.md or .github/instructions/**/*.instructions.md"
        ),
        OutputFormat::Windsurf => ensure!(
            has_prefix(".windsurf/rules") && path.components().count() > 2 && has_extension("md"),
            "windsurf output {name} must write .windsurf/rules/**/*.md"
        ),
        OutputFormat::Cline => ensure!(
            has_prefix(".clinerules")
                && path.components().count() > 1
                && (has_extension("md") || has_extension("txt")),
            "cline output {name} must write .clinerules/**/*.md or .clinerules/**/*.txt"
        ),
    }

    validate_front_matter(name, format, path, front_matter)
}

fn validate_front_matter(
    output: &str,
    format: OutputFormat,
    path: &Path,
    front_matter: &BTreeMap<String, Value>,
) -> Result<()> {
    let value = |key: &str| front_matter.get(key);
    match format {
        OutputFormat::Cursor => {
            if let Some(item) = value("description") {
                ensure_yaml_string(item, output, "description")?;
            }
            if let Some(item) = value("globs") {
                ensure_yaml_string_or_strings(item, output, "globs")?;
            }
            if let Some(item) = value("alwaysApply") {
                ensure!(
                    matches!(item, Value::Bool(_)),
                    "cursor output {output} front_matter.alwaysApply must be a boolean"
                );
            }
        }
        OutputFormat::Copilot if path != Path::new(".github/copilot-instructions.md") => {
            let apply_to = value("applyTo").context(format!(
                "path-specific copilot output {output} requires front_matter.applyTo"
            ))?;
            ensure_yaml_string(apply_to, output, "applyTo")?;
        }
        OutputFormat::Windsurf => {
            let trigger = value("trigger").context(format!(
                "windsurf output {output} requires front_matter.trigger"
            ))?;
            let trigger = ensure_yaml_string(trigger, output, "trigger")?;
            ensure!(
                matches!(trigger, "always_on" | "glob" | "model_decision" | "manual"),
                "windsurf output {output} front_matter.trigger must be always_on, glob, model_decision, or manual"
            );
            if trigger == "glob" {
                let globs = value("globs").context(format!(
                    "windsurf glob output {output} requires front_matter.globs"
                ))?;
                ensure_yaml_string_or_strings(globs, output, "globs")?;
            }
            if trigger == "model_decision" {
                let description = value("description").context(format!(
                    "windsurf model_decision output {output} requires front_matter.description"
                ))?;
                ensure_yaml_string(description, output, "description")?;
            }
        }
        OutputFormat::Cline => {
            if let Some(item) = value("paths") {
                ensure_yaml_string_or_strings(item, output, "paths")?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn ensure_yaml_string<'a>(value: &'a Value, output: &str, key: &str) -> Result<&'a str> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Ok(value),
        Value::String(_) => bail!("output {output} front_matter.{key} may not be empty"),
        _ => bail!("output {output} front_matter.{key} must be a non-empty string"),
    }
}

fn ensure_yaml_string_or_strings(value: &Value, output: &str, key: &str) -> Result<()> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Ok(()),
        Value::Sequence(values)
            if !values.is_empty()
                && values.iter().all(
                    |item| matches!(item, Value::String(value) if !value.trim().is_empty()),
                ) =>
        {
            Ok(())
        }
        _ => bail!(
            "output {output} front_matter.{key} must be a non-empty string or list of non-empty strings"
        ),
    }
}

pub fn compile_project(project: &Project) -> Result<CompiledProject> {
    let condition_results = project
        .rules
        .iter()
        .map(|rule| {
            let result = rule
                .when
                .as_ref()
                .map(|condition| evaluate_condition(&project.root, condition))
                .transpose()?
                .unwrap_or(true);
            Ok((rule.id.clone(), result))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let source_fingerprint = source_fingerprint(project, &condition_results);
    let mut compiled_outputs = BTreeMap::new();
    let mut effective_checks = BTreeMap::<(&str, usize), Vec<String>>::new();

    for (target, output) in &project.outputs {
        validate_templates(project, output, &source_fingerprint)?;
        let applicable = project
            .rules
            .iter()
            .filter(|rule| {
                condition_results[&rule.id]
                    && rule
                        .targets
                        .iter()
                        .any(|item| item == "*" || item == target)
            })
            .collect::<Vec<_>>();
        let mut inapplicable = project
            .rules
            .iter()
            .filter(|rule| {
                !condition_results[&rule.id]
                    && rule
                        .targets
                        .iter()
                        .any(|item| item == "*" || item == target)
            })
            .map(|rule| rule.id.clone())
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
        inapplicable.sort();

        for rule in &selected {
            for index in 0..rule.checks.len() {
                effective_checks
                    .entry((&rule.id, index))
                    .or_default()
                    .push(target.clone());
            }
        }

        let marked_content = render_output(project, output, &selected, &source_fingerprint)?;
        let provenance = build_provenance(project, output, &selected, &marked_content);
        let content = strip_provenance_markers(&marked_content, project, output, &selected);
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
                inapplicable,
                provenance,
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

fn evaluate_condition(root: &Path, condition: &Condition) -> Result<bool> {
    match condition {
        Condition::All(conditions) => Ok(conditions
            .iter()
            .map(|condition| evaluate_condition(root, condition))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .all(|result| result)),
        Condition::Any(conditions) => Ok(conditions
            .iter()
            .map(|condition| evaluate_condition(root, condition))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .any(|result| result)),
        Condition::Not(condition) => Ok(!evaluate_condition(root, condition)?),
        Condition::PathExists { path, kind } => {
            let candidate = root.join(path);
            let canonical = match candidate.canonicalize() {
                Ok(canonical) => canonical,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to resolve path-exists condition {}",
                            path_string(path)
                        )
                    });
                }
            };
            ensure_inside(root, &canonical, "path-exists condition")?;
            Ok(match kind {
                PathKind::Any => true,
                PathKind::File => canonical.is_file(),
                PathKind::Directory => canonical.is_dir(),
            })
        }
    }
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

fn render_output(
    project: &Project,
    output: &Output,
    rules: &[&Rule],
    fingerprint: &str,
) -> Result<String> {
    let front_matter = render_front_matter(&output.front_matter)?;
    let mut context = output_template_context(project, output, fingerprint, &front_matter, "");
    let generated_header = match &output.header {
        Header::Default => generated_header(project, fingerprint),
        Header::Omit => String::new(),
        Header::Template(template) => render_template(template, &context)?,
    };

    let mut rendered_sections = Vec::new();
    let mut current_section: Option<&str> = None;
    let mut current_content = Vec::new();
    for rule in rules {
        if current_section != Some(rule.section.as_str()) {
            if let Some(section_id) = current_section {
                rendered_sections.push(render_section(
                    project,
                    output,
                    &project.sections[section_id],
                    &current_content.join("\n\n"),
                    fingerprint,
                    &front_matter,
                    &generated_header,
                )?);
                current_content.clear();
            }
            current_section = Some(&rule.section);
        }
        current_content.push(mark_rule(
            project,
            output,
            rules,
            rule,
            rule.content.trim_end(),
        ));
    }
    if let Some(section_id) = current_section {
        rendered_sections.push(render_section(
            project,
            output,
            &project.sections[section_id],
            &current_content.join("\n\n"),
            fingerprint,
            &front_matter,
            &generated_header,
        )?);
    }
    let sections = rendered_sections.join("\n\n");
    context.insert("generated_header", generated_header.clone());
    context.insert("sections", sections.clone());

    let rendered = match &output.templates.output {
        Some(template) => {
            let (rendered, placeholders) = render_template_with_placeholders(template, &context)?;
            assert_required_placeholder(&placeholders, "sections", "output template")?;
            rendered
        }
        None => default_output(&front_matter, &generated_header, &output.title, &sections),
    };
    Ok(normalize_output(&rendered))
}

fn validate_templates(project: &Project, output: &Output, fingerprint: &str) -> Result<()> {
    let front_matter = render_front_matter(&output.front_matter)?;
    let mut context = output_template_context(project, output, fingerprint, &front_matter, "");
    let header = match &output.header {
        Header::Default => generated_header(project, fingerprint),
        Header::Omit => String::new(),
        Header::Template(template) => render_template(template, &context)?,
    };
    if let Some(template) = &output.templates.output {
        let (_, placeholders) = render_template_with_placeholders(template, &context)?;
        assert_required_placeholder(&placeholders, "sections", "output template")?;
    }
    if let Some(template) = &output.templates.section {
        let section = project
            .sections
            .values()
            .next()
            .context("project has no sections")?;
        context.insert("generated_header", header);
        context.insert("content", "template validation".to_owned());
        context.insert("section_id", section.id.clone());
        context.insert("section_title", section.title.trim().to_owned());
        context.insert("section_order", section.order.to_string());
        let (_, placeholders) = render_template_with_placeholders(template, &context)?;
        assert_required_placeholder(&placeholders, "content", "section template")?;
    }
    Ok(())
}

fn render_section(
    project: &Project,
    output: &Output,
    section: &Section,
    content: &str,
    fingerprint: &str,
    front_matter: &str,
    generated_header: &str,
) -> Result<String> {
    let mut context =
        output_template_context(project, output, fingerprint, front_matter, generated_header);
    context.insert("content", content.to_owned());
    context.insert("section_id", section.id.clone());
    context.insert("section_title", section.title.trim().to_owned());
    context.insert("section_order", section.order.to_string());
    match &output.templates.section {
        Some(template) => {
            let (rendered, placeholders) = render_template_with_placeholders(template, &context)?;
            assert_required_placeholder(&placeholders, "content", "section template")?;
            Ok(rendered)
        }
        None => Ok(format!("## {}\n\n{content}", section.title.trim())),
    }
}

fn output_template_context(
    project: &Project,
    output: &Output,
    fingerprint: &str,
    front_matter: &str,
    generated_header: &str,
) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("project_name", project.name.clone()),
        ("output_name", output.name.clone()),
        ("output_path", path_string(&output.relative_path)),
        ("title", output.title.trim().to_owned()),
        ("renderer", output.format.name().to_owned()),
        ("fingerprint", fingerprint.to_owned()),
        ("front_matter", front_matter.to_owned()),
        ("generated_header", generated_header.to_owned()),
        ("sections", String::new()),
    ])
}

fn render_template(template: &Template, context: &BTreeMap<&str, String>) -> Result<String> {
    Ok(render_template_with_placeholders(template, context)?.0)
}

fn render_template_with_placeholders(
    template: &Template,
    context: &BTreeMap<&str, String>,
) -> Result<(String, BTreeMap<String, usize>)> {
    let mut rendered = String::new();
    let mut remaining = template.content.as_str();
    let mut placeholders = BTreeMap::new();
    while let Some(start) = remaining.find("{{") {
        let (prefix, after_open) = remaining.split_at(start);
        rendered.push_str(prefix);
        let after_open = &after_open[2..];
        let end = after_open
            .find("}}")
            .context("template has an unclosed {{ placeholder")?;
        let (name, after_close) = after_open.split_at(end);
        let name = name.trim();
        ensure!(!name.is_empty(), "template contains an empty placeholder");
        ensure!(
            name.chars()
                .all(|character| character.is_ascii_lowercase() || character == '_'),
            "template placeholder {name:?} must use lowercase letters and underscores"
        );
        let value = context
            .get(name)
            .with_context(|| format!("template references unknown placeholder {name:?}"))?;
        rendered.push_str(value);
        *placeholders.entry(name.to_owned()).or_insert(0) += 1;
        remaining = &after_close[2..];
    }
    ensure!(
        !remaining.contains("}}"),
        "template has an unmatched }} delimiter"
    );
    rendered.push_str(remaining);
    Ok((rendered, placeholders))
}

fn assert_required_placeholder(
    placeholders: &BTreeMap<String, usize>,
    required: &str,
    kind: &str,
) -> Result<()> {
    ensure!(
        placeholders.get(required) == Some(&1),
        "{kind} must contain exactly one {{{{{required}}}}} placeholder"
    );
    Ok(())
}

fn render_front_matter(front_matter: &BTreeMap<String, Value>) -> Result<String> {
    if front_matter.is_empty() {
        return Ok(String::new());
    }
    let front_matter = front_matter
        .iter()
        .map(|(key, value)| Ok((key.clone(), canonicalize_yaml_value(value)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let body =
        serde_yaml_ng::to_string(&front_matter).context("failed to serialize front matter")?;
    Ok(format!(
        "---\n{}\n---",
        normalize_template(&body).trim_end()
    ))
}

fn canonicalize_yaml_value(value: &Value) -> Result<Value> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(value.clone()),
        Value::Sequence(items) => items
            .iter()
            .map(canonicalize_yaml_value)
            .collect::<Result<Vec<_>>>()
            .map(Value::Sequence),
        Value::Mapping(mapping) => {
            let mut entries = mapping
                .iter()
                .map(|(key, value)| {
                    let key = canonicalize_yaml_value(key)?;
                    let ordering = serde_yaml_ng::to_string(&key)
                        .context("failed to serialize a front matter mapping key")?;
                    Ok((ordering, key, canonicalize_yaml_value(value)?))
                })
                .collect::<Result<Vec<_>>>()?;
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut canonical = serde_yaml_ng::Mapping::new();
            for (_, key, value) in entries {
                canonical.insert(key, value);
            }
            Ok(Value::Mapping(canonical))
        }
        Value::Tagged(tagged) => Ok(Value::Tagged(Box::new(serde_yaml_ng::value::TaggedValue {
            tag: tagged.tag.clone(),
            value: canonicalize_yaml_value(&tagged.value)?,
        }))),
    }
}

fn generated_header(project: &Project, fingerprint: &str) -> String {
    let config = display_relative(&project.root, &project.config_path);
    format!("<!-- Generated by ctcx from {config}. DO NOT EDIT. Fingerprint: {fingerprint} -->")
}

fn default_output(front_matter: &str, header: &str, title: &str, sections: &str) -> String {
    let mut parts = Vec::new();
    if !front_matter.is_empty() {
        parts.push(front_matter.to_owned());
    }
    if !header.is_empty() {
        parts.push(header.to_owned());
    }
    parts.push(format!("# {}", title.trim()));
    if !sections.is_empty() {
        parts.push(sections.to_owned());
    }
    parts.join("\n\n")
}

fn normalize_output(output: &str) -> String {
    format!("{}\n", normalize_template(output).trim_end())
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
    for id in &output.inapplicable {
        let rule = rule_map[id.as_str()];
        if slot.is_none() || rule.slot.as_deref() == slot {
            writeln!(rendered, "  inapplicable {} [condition=false]", rule.id)?;
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
            ExplainStatus::Inapplicable => writeln!(
                rendered,
                "  {target}: inapplicable (condition evaluated false)"
            )?,
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
    } else if output.inapplicable.iter().any(|id| id == rule_id) {
        ExplainStatus::Inapplicable
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
        "version: 1\n\nproject:\n  name: {project_scalar}\n\noutputs:\n  agents:\n    path: AGENTS.md\n    title: Project Agent Instructions\n    format: agents\n  claude:\n    path: CLAUDE.md\n    title: Claude Code Instructions\n    format: claude\n\nsections:\n  - id: workflow\n    title: Workflow\n    order: 100\n\nrules:\n  - id: project-workflow\n    priority: 0\n    targets: [\"*\"]\n    section: workflow\n    order: 100\n    content:\n      file: instructions/project.md\n"
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

fn source_fingerprint(project: &Project, condition_results: &BTreeMap<String, bool>) -> String {
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
    for (rule_id, result) in condition_results {
        hasher.update(b"condition=");
        hasher.update(rule_id.as_bytes());
        hasher.update(b"=");
        hasher.update(if *result { "true" } else { "false" }.as_bytes());
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

fn normalize_template(content: &str) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n")
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

pub(crate) fn normalize_relative(path: &Path) -> Result<PathBuf> {
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

fn locate_mapping_key(root: &Path, path: &Path, parent: &str, key: &str) -> SourceLocation {
    locate_yaml(root, path, |lines| {
        let mut in_parent = false;
        let mut parent_indent = 0;
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if trimmed == format!("{parent}:") {
                in_parent = true;
                parent_indent = indent;
                continue;
            }
            if trimmed.starts_with(&format!("{parent}:")) {
                let key_patterns = [
                    format!("{key}:"),
                    format!("\"{key}\":"),
                    format!("'{key}':"),
                ];
                if let Some(offset) = key_patterns
                    .iter()
                    .find_map(|pattern| line.find(pattern).map(|offset| offset + 1))
                {
                    return Some((index + 1, offset));
                }
                in_parent = true;
                parent_indent = indent;
                continue;
            }
            if in_parent && !trimmed.is_empty() && indent <= parent_indent {
                break;
            }
            if in_parent && trimmed.starts_with(&format!("{key}:")) {
                return Some((index + 1, indent + 1));
            }
        }
        None
    })
}

fn locate_import(root: &Path, path: &Path, import: &Path) -> SourceLocation {
    locate_yaml(root, path, |lines| {
        let mut in_imports = false;
        let mut parent_indent = 0;
        let expected = import.to_string_lossy();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if trimmed.starts_with("imports:") {
                in_imports = true;
                parent_indent = indent;
                if let Some(offset) = line.find("path:") {
                    let value_start = offset + 5;
                    let value = line[value_start..]
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_matches(['\'', '"', ',', '}', ']']);
                    if value == expected {
                        return Some((
                            index + 1,
                            value_start + line[value_start..].len()
                                - line[value_start..].trim_start().len()
                                + 1,
                        ));
                    }
                }
                continue;
            }
            if in_imports && !trimmed.is_empty() && indent <= parent_indent {
                break;
            }
            if in_imports && let Some(offset) = line.find("path:") {
                let value_start = offset + 5;
                let value = line[value_start..]
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches(['\'', '"', ',', '}', ']']);
                if value == expected {
                    return Some((
                        index + 1,
                        value_start + line[value_start..].len()
                            - line[value_start..].trim_start().len()
                            + 1,
                    ));
                }
            }
        }
        None
    })
}

fn locate_id(root: &Path, path: &Path, parent: &str, id: &str) -> SourceLocation {
    locate_yaml(root, path, |lines| {
        let mut in_parent = false;
        let mut parent_indent = 0;
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if trimmed.starts_with(&format!("{parent}:")) {
                in_parent = true;
                parent_indent = indent;
            }
            if in_parent
                && index > 0
                && !trimmed.is_empty()
                && indent <= parent_indent
                && !trimmed.starts_with(&format!("{parent}:"))
            {
                break;
            }
            if in_parent {
                let mut offset = 0;
                while let Some(found) = line[offset..].find("id:") {
                    let key = offset + found;
                    let prefix = line[..key].trim_end();
                    let before_ok = prefix.is_empty() || prefix.ends_with(['-', '{', '[']);
                    if indent > parent_indent + 2 && prefix.is_empty() {
                        offset = key + 3;
                        continue;
                    }
                    let value = line[key + 3..].trim_start();
                    let value = value.trim_start_matches(['\'', '"']);
                    let end = value
                        .find([',', '}', ']', '\'', '"', ' '])
                        .unwrap_or(value.len());
                    if before_ok && &value[..end] == id {
                        return Some((index + 1, key + 1));
                    }
                    offset = key + 3;
                }
            }
        }
        None
    })
}

fn locate_rule_content(root: &Path, path: &Path, id: &str) -> SourceLocation {
    let rule = locate_id(root, path, "rules", id);
    locate_yaml(root, path, |lines| {
        for (index, line) in lines.iter().enumerate().skip(rule.line.saturating_sub(1)) {
            let search_from = if index + 1 == rule.line {
                rule.column.saturating_sub(1)
            } else {
                0
            };
            let tail = &line[search_from.min(line.len())..];
            let trimmed = tail.trim_start();
            if index + 1 > rule.line && (trimmed.starts_with("- id:") || trimmed.contains("{ id:"))
            {
                break;
            }
            let inline = tail.find("inline:").map(|offset| (offset, true));
            let file = tail.find("file:").map(|offset| (offset, false));
            if let Some((offset, is_inline)) = inline.into_iter().chain(file).min_by_key(|x| x.0) {
                let key_column = search_from + offset;
                let value = line[key_column + if is_inline { 7 } else { 5 }..].trim_start();
                let block = is_inline && (value.starts_with('|') || value.starts_with('>'));
                let value_column = line.len() - value.len() + 1;
                if block {
                    let mut content_index = index + 1;
                    while content_index < lines.len() && lines[content_index].trim().is_empty() {
                        content_index += 1;
                    }
                    if let Some(content_line) = lines.get(content_index) {
                        let next_indent = content_line.len() - content_line.trim_start().len();
                        return Some((content_index + 1, next_indent + 1));
                    }
                }
                return Some((index + 1 + usize::from(block), value_column));
            }
        }
        None
    })
}

fn locate_yaml(
    root: &Path,
    path: &Path,
    finder: impl FnOnce(&[&str]) -> Option<(usize, usize)>,
) -> SourceLocation {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines = text.lines().collect::<Vec<_>>();
    let (line, column) = finder(&lines).unwrap_or((1, 1));
    SourceLocation {
        path: display_relative(root, path),
        line,
        column,
    }
}

fn build_provenance(
    project: &Project,
    output: &Output,
    rules: &[&Rule],
    content: &str,
) -> Vec<ProvenanceRange> {
    let sanitized = strip_provenance_markers(content, project, output, rules);
    let total_lines = sanitized.lines().count().max(1);
    let mut ranges = Vec::new();
    for rule in rules {
        let start_marker = provenance_marker(project, output, rules, rule, "start");
        let end_marker = provenance_marker(project, output, rules, rule, "end");
        let mut cursor = 0;
        while let Some(relative) = content[cursor..].find(&start_marker) {
            let start_byte = cursor + relative;
            let after_start = start_byte + start_marker.len();
            let Some(end_relative) = content[after_start..].find(&end_marker) else {
                break;
            };
            let end_byte = after_start + end_relative;
            let start_line = sanitized
                [..strip_marker_offset(content, start_byte, project, output, rules)]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            let end_line = sanitized
                [..strip_marker_offset(content, end_byte, project, output, rules)]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1;
            ranges.push(ProvenanceRange {
                start_line,
                end_line,
                kind: ProvenanceKind::RuleContent,
                rule: Some(rule.id.clone()),
                section: Some(rule.section.clone()),
                sources: {
                    let mut sources = vec![rule.location.clone(), rule.content_location.clone()];
                    for generated_line in start_line..=end_line {
                        sources.extend(template_sources_for_line(
                            output,
                            sanitized.lines().nth(generated_line - 1).unwrap_or(""),
                            true,
                            true,
                        ));
                    }
                    sources.extend(template_placeholder_sources(output, true));
                    sources.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.line.cmp(&b.line)));
                    sources.dedup();
                    sources
                },
            });
            cursor = end_byte + end_marker.len();
        }
    }
    ranges.sort_by_key(|range| range.start_line);
    let mut complete = Vec::new();
    let mut next = 1;
    for range in ranges {
        if next < range.start_line {
            for generated_line in next..range.start_line {
                let mut sources = template_sources_for_line(
                    output,
                    sanitized.lines().nth(generated_line - 1).unwrap_or(""),
                    true,
                    false,
                );
                if sanitized
                    .lines()
                    .nth(generated_line - 1)
                    .is_some_and(|line| line.trim_start().starts_with("## "))
                    && let Some(section) = range
                        .section
                        .as_ref()
                        .and_then(|id| project.sections.get(id))
                {
                    sources.push(section.location.clone());
                }
                complete.push(ProvenanceRange {
                    start_line: generated_line,
                    end_line: generated_line,
                    kind: ProvenanceKind::Output,
                    rule: None,
                    section: range.section.clone(),
                    sources,
                });
            }
        }
        next = range.end_line + 1;
        complete.push(range);
    }
    if next <= total_lines {
        let section = rules.last().map(|rule| rule.section.clone());
        for generated_line in next..=total_lines {
            let mut sources = template_sources_for_line(
                output,
                sanitized.lines().nth(generated_line - 1).unwrap_or(""),
                true,
                false,
            );
            if sanitized
                .lines()
                .nth(generated_line - 1)
                .is_some_and(|line| line.trim_start().starts_with("## "))
                && let Some(value) = section.as_ref().and_then(|id| project.sections.get(id))
            {
                sources.push(value.location.clone());
            }
            complete.push(ProvenanceRange {
                start_line: generated_line,
                end_line: generated_line,
                kind: ProvenanceKind::Output,
                rule: None,
                section: section.clone(),
                sources,
            });
        }
    }
    complete
}

fn template_sources_for_line(
    output: &Output,
    generated_line: &str,
    include_section: bool,
    include_section_placeholder: bool,
) -> Vec<SourceLocation> {
    let mut sources = Vec::new();
    let templates = [
        match &output.header {
            Header::Template(template) => Some(template),
            _ => None,
        },
        output.templates.output.as_ref(),
        include_section
            .then_some(output.templates.section.as_ref())
            .flatten(),
    ];
    for template in templates.into_iter().flatten() {
        for (index, template_line) in template.content.lines().enumerate() {
            let template_text = template_line.trim();
            if template_line_matches(template_text, generated_line.trim()) {
                sources.push(
                    template
                        .source_locations
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| template.location.clone()),
                );
            }
        }
    }
    if sources.is_empty() {
        sources = template_placeholder_sources(output, include_section_placeholder);
    }
    if sources.is_empty() {
        sources.push(output.location.clone());
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.line.cmp(&b.line)));
    sources.dedup();
    sources
}

fn template_line_matches(template_line: &str, generated_line: &str) -> bool {
    if template_line.is_empty() {
        return false;
    }
    if !template_line.contains("{{") {
        return template_line == generated_line || generated_line.contains(template_line);
    }
    let mut remaining = template_line;
    let mut saw_literal = false;
    while let Some(start) = remaining.find("{{") {
        let literal = remaining[..start].trim();
        if !literal.is_empty() {
            saw_literal = true;
            if generated_line.contains(literal) {
                return true;
            }
        }
        let Some(end) = remaining[start + 2..].find("}}") else {
            break;
        };
        remaining = &remaining[start + 2 + end + 2..];
    }
    let literal = remaining.trim();
    saw_literal && !literal.is_empty() && generated_line.contains(literal)
}

fn template_placeholder_sources(output: &Output, include_section: bool) -> Vec<SourceLocation> {
    let templates = [
        match &output.header {
            Header::Template(template) => Some(template),
            _ => None,
        },
        output.templates.output.as_ref(),
        include_section
            .then_some(output.templates.section.as_ref())
            .flatten(),
    ];
    let mut sources = Vec::new();
    for template in templates.into_iter().flatten() {
        for (index, line) in template.content.lines().enumerate() {
            if line.contains("{{content}}") || line.contains("{{sections}}") {
                sources.push(
                    template
                        .source_locations
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| template.location.clone()),
                );
            }
        }
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.line.cmp(&b.line)));
    sources.dedup();
    sources
}

fn provenance_marker(
    project: &Project,
    output: &Output,
    rules: &[&Rule],
    rule: &Rule,
    edge: &str,
) -> String {
    let mut marker = format!(
        "__ctcx_provenance_{}_{}_{}__",
        sha256(rule.content.as_bytes()),
        edge,
        rule.id
    );
    while marker_collides(project, output, rules, &marker) {
        marker.push('x');
    }
    marker
}

fn mark_rule(
    project: &Project,
    output: &Output,
    rules: &[&Rule],
    rule: &Rule,
    content: &str,
) -> String {
    format!(
        "{}{}{}",
        provenance_marker(project, output, rules, rule, "start"),
        content,
        provenance_marker(project, output, rules, rule, "end")
    )
}

fn strip_provenance_markers(
    content: &str,
    project: &Project,
    output: &Output,
    rules: &[&Rule],
) -> String {
    let mut sanitized = content.to_owned();
    for rule in rules {
        for edge in ["start", "end"] {
            let marker = provenance_marker(project, output, rules, rule, edge);
            sanitized = sanitized.replace(&marker, "");
        }
    }
    sanitized
}

fn strip_marker_offset(
    content: &str,
    byte_offset: usize,
    project: &Project,
    output: &Output,
    rules: &[&Rule],
) -> usize {
    strip_provenance_markers(&content[..byte_offset], project, output, rules).len()
}

fn marker_collides(project: &Project, output: &Output, rules: &[&Rule], marker: &str) -> bool {
    rules.iter().any(|rule| rule.content.contains(marker))
        || project
            .sections
            .values()
            .any(|section| section.title.contains(marker))
        || project.name.contains(marker)
        || output.name.contains(marker)
        || path_string(&output.relative_path).contains(marker)
        || output.format.name().contains(marker)
        || output.title.contains(marker)
        || serde_yaml_ng::to_string(&output.front_matter).is_ok_and(|value| value.contains(marker))
        || output
            .templates
            .output
            .as_ref()
            .is_some_and(|template| template.content.contains(marker))
        || output
            .templates
            .section
            .as_ref()
            .is_some_and(|template| template.content.contains(marker))
        || matches!(&output.header, Header::Template(template) if template.content.contains(marker))
}

fn locate_output_template(
    root: &Path,
    path: &Path,
    output: &str,
    raw: &RawTemplate,
) -> SourceLocation {
    let output_location = locate_mapping_key(root, path, "outputs", output);
    locate_yaml(root, path, |lines| {
        let start = output_location.line.saturating_sub(1);
        let base_indent = lines
            .get(start)
            .map(|line| line.len() - line.trim_start().len())
            .unwrap_or(0);
        for (index, line) in lines.iter().enumerate().skip(start) {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if index > start && !trimmed.is_empty() && indent <= base_indent {
                break;
            }
            let key = if raw.file.is_some() {
                "file:"
            } else {
                "inline:"
            };
            if let Some(column) = line.find(key) {
                let matches_value = raw
                    .file
                    .as_ref()
                    .is_some_and(|value| line.contains(&value.to_string_lossy().to_string()))
                    || raw.inline.as_ref().is_some_and(|value| {
                        let prefix = value.lines().next().unwrap_or("");
                        prefix.is_empty() || line.contains(prefix)
                    });
                if matches_value {
                    return Some((index + 1, column + 1));
                }
            }
        }
        None
    })
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
