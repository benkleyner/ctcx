use crate::compiler::{CompiledProject, Header, Project, ProvenanceKind, SourceLocation};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;
use std::path::Path;

pub const GRAPH_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum GraphNodeKind {
    Document,
    ContentFile,
    TemplateFile,
    Rule,
    Section,
    Output,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum GraphEdgeKind {
    Imports,
    Defines,
    ReadsContent,
    UsesTemplate,
    Targets,
    Applies,
    Inapplicable,
    SuppressedBy,
    BelongsToSection,
    RendersTo,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphNode {
    pub id: String,
    pub kind: GraphNodeKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: GraphEdgeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GraphStatus {
    pub output: String,
    pub rule: String,
    pub status: GraphRuleStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GraphRuleStatus {
    Applied,
    Suppressed,
    Inapplicable,
    NotTargeted,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DependencyGraph {
    pub schema_version: u32,
    pub project: String,
    pub root_config: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub statuses: Vec<GraphStatus>,
}

pub fn build_dependency_graph(project: &Project, compiled: &CompiledProject) -> DependencyGraph {
    let mut nodes = BTreeMap::<String, GraphNode>::new();
    let mut edges = Vec::new();
    let mut statuses = Vec::new();
    for document in &project.documents {
        let id = file_id("document", &document.location.path);
        nodes.insert(
            id.clone(),
            GraphNode {
                id: id.clone(),
                kind: GraphNodeKind::Document,
                label: document.location.path.clone(),
                location: Some(document.location.clone()),
            },
        );
        for import in &document.imports {
            let path = relative(
                project,
                &import
                    .path
                    .canonicalize()
                    .unwrap_or_else(|_| import.path.clone()),
            );
            edges.push(GraphEdge {
                from: id.clone(),
                to: file_id("document", &path),
                kind: GraphEdgeKind::Imports,
                output: None,
                location: Some(import.location.clone()),
            });
        }
    }
    for section in project.sections.values() {
        let id = format!("section:{}", section.id);
        nodes.insert(
            id.clone(),
            GraphNode {
                id: id.clone(),
                kind: GraphNodeKind::Section,
                label: section.id.clone(),
                location: Some(section.location.clone()),
            },
        );
        edges.push(edge(
            file_id("document", &relative(project, &section.source)),
            id,
            GraphEdgeKind::Defines,
            None,
        ));
    }
    for rule in &project.rules {
        let id = format!("rule:{}", rule.id);
        nodes.insert(
            id.clone(),
            GraphNode {
                id: id.clone(),
                kind: GraphNodeKind::Rule,
                label: rule.id.clone(),
                location: Some(rule.location.clone()),
            },
        );
        edges.push(edge(
            file_id("document", &relative(project, &rule.source)),
            id.clone(),
            GraphEdgeKind::Defines,
            None,
        ));
        edges.push(edge(
            id.clone(),
            format!("section:{}", rule.section),
            GraphEdgeKind::BelongsToSection,
            None,
        ));
        if let Some(path) = &rule.content_source {
            let path = relative(project, path);
            let file = file_id("content", &path);
            nodes.entry(file.clone()).or_insert(GraphNode {
                id: file.clone(),
                kind: GraphNodeKind::ContentFile,
                label: path,
                location: Some(rule.content_location.clone()),
            });
            edges.push(edge(id.clone(), file, GraphEdgeKind::ReadsContent, None));
        }
        for target in &rule.targets {
            if target == "*" {
                for output in project.outputs.keys() {
                    edges.push(edge(
                        id.clone(),
                        format!("output:{output}"),
                        GraphEdgeKind::Targets,
                        Some(output.clone()),
                    ));
                }
            } else {
                edges.push(edge(
                    id.clone(),
                    format!("output:{target}"),
                    GraphEdgeKind::Targets,
                    Some(target.clone()),
                ));
            }
        }
    }
    for (name, output) in &project.outputs {
        let id = format!("output:{name}");
        nodes.insert(
            id.clone(),
            GraphNode {
                id: id.clone(),
                kind: GraphNodeKind::Output,
                label: output.relative_path.display().to_string(),
                location: Some(output.location.clone()),
            },
        );
        edges.push(edge(
            file_id("document", &relative(project, &project.config_path)),
            id.clone(),
            GraphEdgeKind::Defines,
            None,
        ));
        let templates = [
            output.templates.output.as_ref(),
            output.templates.section.as_ref(),
            match &output.header {
                Header::Template(template) => Some(template),
                _ => None,
            },
        ];
        for template in templates.into_iter().flatten() {
            if let Some(path) = &template.source {
                let path = relative(project, path);
                let file = file_id("template", &path);
                nodes.entry(file.clone()).or_insert(GraphNode {
                    id: file.clone(),
                    kind: GraphNodeKind::TemplateFile,
                    label: path,
                    location: Some(template.location.clone()),
                });
                edges.push(edge(id.clone(), file, GraphEdgeKind::UsesTemplate, None));
            }
        }
        let result = &compiled.outputs[name];
        for rule in &project.rules {
            if result.applied.iter().any(|id| id == &rule.id) {
                statuses.push(GraphStatus {
                    output: name.clone(),
                    rule: rule.id.clone(),
                    status: GraphRuleStatus::Applied,
                    winner: None,
                });
            } else if let Some(item) = result.suppressed.iter().find(|item| item.rule == rule.id) {
                statuses.push(GraphStatus {
                    output: name.clone(),
                    rule: rule.id.clone(),
                    status: GraphRuleStatus::Suppressed,
                    winner: Some(item.winner.clone()),
                });
            } else if result.inapplicable.iter().any(|id| id == &rule.id) {
                statuses.push(GraphStatus {
                    output: name.clone(),
                    rule: rule.id.clone(),
                    status: GraphRuleStatus::Inapplicable,
                    winner: None,
                });
            } else {
                statuses.push(GraphStatus {
                    output: name.clone(),
                    rule: rule.id.clone(),
                    status: GraphRuleStatus::NotTargeted,
                    winner: None,
                });
            }
        }
        for rule in &result.applied {
            edges.push(edge(
                format!("rule:{rule}"),
                id.clone(),
                GraphEdgeKind::Applies,
                Some(name.clone()),
            ));
            edges.push(edge(
                format!("rule:{rule}"),
                id.clone(),
                GraphEdgeKind::RendersTo,
                Some(name.clone()),
            ));
        }
        for rule in &result.inapplicable {
            edges.push(edge(
                format!("rule:{rule}"),
                id.clone(),
                GraphEdgeKind::Inapplicable,
                Some(name.clone()),
            ));
        }
        for suppression in &result.suppressed {
            edges.push(edge(
                format!("rule:{}", suppression.rule),
                format!("rule:{}", suppression.winner),
                GraphEdgeKind::SuppressedBy,
                Some(name.clone()),
            ));
        }
    }
    let mut nodes = nodes.into_values().collect::<Vec<_>>();
    nodes.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.id.cmp(&b.id)));
    edges.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.from.cmp(&b.from))
            .then_with(|| a.to.cmp(&b.to))
            .then_with(|| a.output.cmp(&b.output))
    });
    statuses.sort_by(|a, b| a.output.cmp(&b.output).then_with(|| a.rule.cmp(&b.rule)));
    DependencyGraph {
        schema_version: GRAPH_SCHEMA_VERSION,
        project: project.name.clone(),
        root_config: relative(project, &project.config_path),
        nodes,
        edges,
        statuses,
    }
}

pub fn render_graph_text(graph: &DependencyGraph) -> String {
    let labels = graph
        .nodes
        .iter()
        .map(|node| (&node.id, &node.label))
        .collect::<BTreeMap<_, _>>();
    let mut out = format!("project {} ({})\n", graph.project, graph.root_config);
    for edge in &graph.edges {
        let suffix = edge
            .output
            .as_ref()
            .map(|value| format!(" [output={value}]"))
            .unwrap_or_default();
        writeln!(
            out,
            "  {} --{}--> {}{}",
            labels.get(&edge.from).copied().unwrap_or(&edge.from),
            edge_name(edge.kind),
            labels.get(&edge.to).copied().unwrap_or(&edge.to),
            suffix
        )
        .unwrap();
    }
    for status in &graph.statuses {
        writeln!(
            out,
            "  status output:{} rule:{} = {}{}",
            status.output,
            status.rule,
            status_name(status.status),
            status
                .winner
                .as_ref()
                .map(|winner| format!(" (winner={winner})"))
                .unwrap_or_default()
        )
        .unwrap();
    }
    out
}

pub fn render_graph_dot(graph: &DependencyGraph) -> String {
    let mut out = String::from("digraph ctcx {\n  rankdir=LR;\n");
    for node in &graph.nodes {
        writeln!(
            out,
            "  \"{}\" [label=\"{}\", shape={}];",
            dot(&node.id),
            dot(&node.label),
            match node.kind {
                GraphNodeKind::Output => "box",
                GraphNodeKind::Rule => "ellipse",
                GraphNodeKind::Section => "folder",
                _ => "note",
            }
        )
        .unwrap();
    }
    for edge in &graph.edges {
        writeln!(
            out,
            "  \"{}\" -> \"{}\" [label=\"{}\"];",
            dot(&edge.from),
            dot(&edge.to),
            edge_name(edge.kind)
        )
        .unwrap();
    }
    for status in &graph.statuses {
        writeln!(
            out,
            "  \"rule:{}\" -> \"output:{}\" [style=dashed,label=\"status:{}\"];",
            dot(&status.rule),
            dot(&status.output),
            status_name(status.status)
        )
        .unwrap();
    }
    out.push_str("}\n");
    out
}

pub fn explain_line(
    project: &Project,
    compiled: &CompiledProject,
    output_path: &Path,
    line: usize,
) -> Result<String> {
    ensure!(line > 0, "line must be a positive 1-based integer");
    let normalized = crate::compiler::normalize_relative(output_path)
        .with_context(|| format!("invalid output path {}", output_path.display()))?
        .to_string_lossy()
        .replace('\\', "/");
    let matches = compiled
        .outputs
        .values()
        .filter(|output| output.relative_path.to_string_lossy().replace('\\', "/") == normalized)
        .collect::<Vec<_>>();
    ensure!(!matches.is_empty(), "unknown output path {normalized}");
    ensure!(matches.len() == 1, "ambiguous output path {normalized}");
    let output = matches[0];
    let line_count = output.content.lines().count().max(1);
    ensure!(
        line <= line_count,
        "line {line} is outside {normalized}; valid range is 1..={line_count}"
    );
    let range = output
        .provenance
        .iter()
        .find(|range| line >= range.start_line && line <= range.end_line)
        .context("generated line has no provenance")?;
    let text = output.content.lines().nth(line - 1).unwrap_or("");
    let mut out = format!(
        "{}:{}\n  generated: {:?}\n  range: {}-{}\n  kind: {:?}\n",
        normalized, line, text, range.start_line, range.end_line, range.kind
    );
    if let Some(rule) = &range.rule {
        let source_rule = project
            .rules
            .iter()
            .find(|candidate| &candidate.id == rule)
            .unwrap();
        writeln!(out, "  rule: {} (applied)", rule)?;
        writeln!(out, "  section: {}", source_rule.section)?;
    }
    for source in &range.sources {
        let mut source_line = source.line;
        let mut source_column = source.column;
        if range.kind == ProvenanceKind::RuleContent
            && let Some(rule_id) = &range.rule
            && let Some(rule) = project
                .rules
                .iter()
                .find(|candidate| &candidate.id == rule_id)
            && rule
                .content_source
                .as_ref()
                .is_some_and(|path| relative(project, path) == source.path)
        {
            source_line += line - range.start_line;
            if let Ok(content) = fs::read_to_string(project.root.join(&source.path)) {
                source_column = content
                    .lines()
                    .nth(source_line.saturating_sub(1))
                    .map(|value| value.len() - value.trim_start().len() + 1)
                    .unwrap_or(source_column);
            }
        }
        writeln!(
            out,
            "  source: {}:{}:{}",
            source.path, source_line, source_column
        )?;
    }
    Ok(out)
}

fn edge(from: String, to: String, kind: GraphEdgeKind, output: Option<String>) -> GraphEdge {
    GraphEdge {
        from,
        to,
        kind,
        output,
        location: None,
    }
}
fn file_id(kind: &str, path: &str) -> String {
    format!("{kind}:{path}")
}
fn relative(project: &Project, path: &Path) -> String {
    path.strip_prefix(&project.root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
fn dot(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn edge_name(kind: GraphEdgeKind) -> &'static str {
    match kind {
        GraphEdgeKind::Imports => "imports",
        GraphEdgeKind::Defines => "defines",
        GraphEdgeKind::ReadsContent => "reads-content",
        GraphEdgeKind::UsesTemplate => "uses-template",
        GraphEdgeKind::Targets => "targets",
        GraphEdgeKind::Applies => "applies",
        GraphEdgeKind::Inapplicable => "inapplicable",
        GraphEdgeKind::SuppressedBy => "suppressed-by",
        GraphEdgeKind::BelongsToSection => "belongs-to-section",
        GraphEdgeKind::RendersTo => "renders-to",
    }
}

fn status_name(status: GraphRuleStatus) -> &'static str {
    match status {
        GraphRuleStatus::Applied => "applied",
        GraphRuleStatus::Suppressed => "suppressed",
        GraphRuleStatus::Inapplicable => "inapplicable",
        GraphRuleStatus::NotTargeted => "not-targeted",
    }
}
