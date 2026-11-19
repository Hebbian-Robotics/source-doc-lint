use std::path::{Component, Path, PathBuf};

use anyhow::{Result, ensure};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentationKind {
    Docstring,
    DocComment,
    DocAttribute,
    SchemaDescription,
}

impl DocumentationKind {
    const ALL: [Self; 4] = [
        Self::Docstring,
        Self::DocComment,
        Self::DocAttribute,
        Self::SchemaDescription,
    ];
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Configuration {
    reason: Option<String>,
    exclude: Vec<String>,
    classification: Option<ClassificationConfiguration>,
    public_interface: Option<PublicInterfaceConfiguration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassificationConfiguration {
    labels: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    extra_directives: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicInterfaceConfiguration {
    directories: Vec<PathBuf>,
    #[serde(default = "all_documentation_kinds")]
    kinds: Vec<DocumentationKind>,
}

fn all_documentation_kinds() -> Vec<DocumentationKind> {
    DocumentationKind::ALL.to_vec()
}

pub struct Policy {
    pub reason: String,
    pub exclude: GlobSet,
    pub classification: Option<Classification>,
    pub public_interface: Option<PublicInterface>,
}

pub struct Classification {
    pub label: Regex,
    pub exclude: GlobSet,
    pub extra_directives: Vec<Regex>,
}

pub struct PublicInterface {
    directories: Vec<PathBuf>,
    pub kinds: Vec<DocumentationKind>,
}

impl PublicInterface {
    pub fn allows(&self, project_path: &Path) -> bool {
        self.directories
            .iter()
            .any(|directory| project_path.starts_with(directory))
    }
}

fn compile_globs(patterns: Vec<String>) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(&pattern)?);
    }
    Ok(builder.build()?)
}

impl TryFrom<Configuration> for Policy {
    type Error = anyhow::Error;

    fn try_from(configuration: Configuration) -> Result<Self> {
        Ok(Self {
            reason: configuration.reason.unwrap_or_else(|| {
                "Follow the comment and documentation rules configured for this project.".into()
            }),
            exclude: compile_globs(configuration.exclude)?,
            classification: configuration
                .classification
                .map(Classification::try_from)
                .transpose()?,
            public_interface: configuration
                .public_interface
                .map(PublicInterface::try_from)
                .transpose()?,
        })
    }
}

impl TryFrom<ClassificationConfiguration> for Classification {
    type Error = anyhow::Error;

    fn try_from(configuration: ClassificationConfiguration) -> Result<Self> {
        ensure!(
            !configuration.labels.is_empty(),
            "classification.labels must contain at least one label"
        );
        ensure!(
            configuration.labels.iter().all(|label| {
                !label.is_empty()
                    && label.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                    })
            }),
            "classification labels may contain letters, digits, underscores, and hyphens"
        );
        let labels = configuration
            .labels
            .iter()
            .map(|label| regex::escape(label))
            .collect::<Vec<_>>()
            .join("|");
        Ok(Self {
            label: Regex::new(&format!(
                r"^(?://|/\*+|#)\s*(?:\*\s*)?(?:{labels}):\s+[^\s*]"
            ))?,
            exclude: compile_globs(configuration.exclude)?,
            extra_directives: configuration
                .extra_directives
                .iter()
                .map(|pattern| Regex::new(pattern))
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TryFrom<PublicInterfaceConfiguration> for PublicInterface {
    type Error = anyhow::Error;

    fn try_from(configuration: PublicInterfaceConfiguration) -> Result<Self> {
        ensure!(
            configuration.directories.iter().all(|directory| {
                directory.components().count() > 0
                    && directory
                        .components()
                        .all(|component| matches!(component, Component::Normal(_)))
            }),
            "public-interface directories must be project-relative paths without parent components"
        );
        ensure!(
            !configuration.kinds.is_empty(),
            "public-interface.kinds must contain at least one documentation kind"
        );
        Ok(Self {
            directories: configuration.directories,
            kinds: configuration.kinds,
        })
    }
}
