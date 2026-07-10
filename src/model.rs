use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const SCHEMA_VERSION: u32 = 1;
pub const MANIFEST_VERSION: u32 = 1;
pub const DEFAULT_ORDER: i32 = 1000;

fn default_order() -> i32 {
    DEFAULT_ORDER
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawDocument {
    pub version: u32,
    #[serde(default)]
    pub project: Option<RawProject>,
    #[serde(default)]
    pub imports: Vec<RawImport>,
    #[serde(default)]
    pub outputs: Option<BTreeMap<String, RawOutput>>,
    #[serde(default)]
    pub sections: Vec<RawSection>,
    #[serde(default)]
    pub rules: Vec<RawRule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawProject {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawImport {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawOutput {
    pub path: PathBuf,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSection {
    pub id: String,
    pub title: String,
    #[serde(default = "default_order")]
    pub order: i32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRule {
    pub id: String,
    #[serde(default)]
    pub slot: Option<String>,
    #[serde(default)]
    pub priority: i32,
    pub targets: Vec<String>,
    pub section: String,
    #[serde(default = "default_order")]
    pub order: i32,
    pub content: RawContent,
    #[serde(default)]
    pub checks: Vec<RawCheck>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RawCheck {
    PackageScript {
        manifest: PathBuf,
        script: String,
    },
    PathExists {
        path: PathBuf,
        #[serde(default)]
        kind: RawPathKind,
    },
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RawPathKind {
    #[default]
    Any,
    File,
    Directory,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawContent {
    #[serde(default)]
    pub inline: Option<String>,
    #[serde(default)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub schema_version: u32,
    pub compiler_version: String,
    pub root_config: String,
    pub source_fingerprint: String,
    pub dependencies: Vec<ManifestDependency>,
    pub outputs: BTreeMap<String, ManifestOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManifestDependency {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManifestOutput {
    pub path: String,
    pub sha256: String,
    pub rules: Vec<String>,
    pub suppressed: Vec<ManifestSuppressed>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManifestSuppressed {
    pub rule: String,
    pub winner: String,
    pub slot: String,
}
