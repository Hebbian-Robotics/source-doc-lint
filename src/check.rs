mod python;
mod typescript;

use anyhow::{Context, Result};
use ast_grep_core::Pattern;
use ast_grep_language::{LanguageExt, SupportLang};
use serde::Serialize;
use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use crate::{
    policy::{DocumentationKind, Policy},
    syntax::{Language, SyntaxNode, is_comment, is_ordinary_comment},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub file: PathBuf,
    pub rule_id: String,
    pub text: String,
    pub language: Language,
    pub range: SourceRange,
    pub action: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub start: Position,
    end: Position,
    pub byte_offset: ByteOffset,
}

#[derive(Serialize)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

#[derive(Serialize)]
pub struct ByteOffset {
    pub start: usize,
    end: usize,
}

struct Source<'source> {
    path: &'source Path,
    text: &'source str,
    language: Language,
    line_starts: Vec<usize>,
}

impl Source<'_> {
    fn position(&self, offset: usize) -> Position {
        let line = self.line_starts.partition_point(|&start| start <= offset) - 1;
        Position {
            line,
            column: self.text[self.line_starts[line]..offset].chars().count(),
        }
    }

    fn finding(&self, span: Range<usize>, rule_id: String, action: &'static str) -> Finding {
        Finding {
            file: self.path.to_owned(),
            rule_id,
            language: self.language,
            text: self.text[span.clone()].to_owned(),
            range: SourceRange {
                start: self.position(span.start),
                end: self.position(span.end),
                byte_offset: ByteOffset {
                    start: span.start,
                    end: span.end,
                },
            },
            action,
        }
    }
}

struct Documentation {
    span: Range<usize>,
    kind: DocumentationKind,
    rule_id: &'static str,
}

impl Documentation {
    fn finding(self, source: &Source<'_>) -> Finding {
        let action = match self.kind {
            DocumentationKind::SchemaDescription => {
                "Put consumer-facing descriptions in the owning public schema. Use a nearby implementation comment for rationale."
            }
            _ => {
                "Remove redundant documentation or keep the useful explanation as a nearby implementation comment."
            }
        };
        source.finding(self.span, self.rule_id.into(), action)
    }
}

pub fn check_source(
    policy: &Policy,
    language: Language,
    path: &Path,
    project_path: &Path,
    text: &str,
) -> Result<Vec<Finding>> {
    let classification = policy
        .classification
        .as_ref()
        .filter(|rule| !rule.exclude.is_match(project_path));
    let documentation = policy
        .public_interface
        .as_ref()
        .filter(|rule| !rule.allows(project_path));
    if classification.is_none() && documentation.is_none() {
        return Ok(Vec::new());
    }
    let source = Source {
        path,
        text,
        language,
        line_starts: std::iter::once(0)
            .chain(text.match_indices('\n').map(|(offset, _)| offset + 1))
            .collect(),
    };
    let parsed = language.parser().ast_grep(text);
    let root = parsed.root();
    let mut findings = Vec::new();
    if let Some(rule) = classification {
        let mut previous_standalone = None;
        for node in root
            .dfs()
            .filter(|node| is_ordinary_comment(language, node))
        {
            let comment = node.text();
            if language.is_directive(&comment)
                || rule
                    .extra_directives
                    .iter()
                    .any(|directive| directive.is_match(&comment))
            {
                continue;
            }
            let span = node.range();
            let (line, column) = node.start_pos().byte_point();
            let indentation = &text[span.start - column..span.start];
            let standalone = (comment.starts_with("//") || comment.starts_with('#'))
                && indentation.bytes().all(|byte| matches!(byte, b' ' | b'\t'));
            let continues_group = standalone
                && previous_standalone
                    == line
                        .checked_sub(1)
                        .map(|previous_line| (previous_line, indentation))
                && previous_standalone.is_some();
            if !continues_group && !rule.label.is_match(&comment) {
                findings.push(source.finding(
                    span,
                    format!("explanatory-comment-{}", language.rule_suffix()),
                    "Remove this comment or label the reason it must remain.",
                ));
            }
            previous_standalone = standalone.then_some((node.end_pos().line(), indentation));
        }
    }
    if let Some(rule) = documentation {
        let kinds = &rule.kinds;
        if kinds.contains(&DocumentationKind::DocComment) {
            let rule_id = match language {
                Language::Rust => "documentation-rust",
                Language::TypeScript => "documentation-typescript",
                Language::Tsx => "documentation-tsx",
                Language::Python => "documentation-python",
            };
            for node in root
                .dfs()
                .filter(|node| is_comment(node) && language.is_doc_comment(&node.text()))
            {
                findings.push(
                    Documentation {
                        span: node.range(),
                        kind: DocumentationKind::DocComment,
                        rule_id,
                    }
                    .finding(&source),
                );
            }
        }
        let detected = match language {
            Language::Python => python::detect(&root, text, kinds)
                .context("parsing Python documentation and schema declarations")?,
            Language::Rust => rust_attributes(&root, kinds),
            Language::TypeScript | Language::Tsx
                if kinds.contains(&DocumentationKind::SchemaDescription) =>
            {
                typescript::detect(&root).context("detecting TypeScript schema descriptions")?
            }
            Language::TypeScript | Language::Tsx => Vec::new(),
        };
        findings.extend(detected.into_iter().map(|item| item.finding(&source)));
    }
    Ok(findings)
}

fn rust_attributes(root: &SyntaxNode<'_>, kinds: &[DocumentationKind]) -> Vec<Documentation> {
    use std::sync::LazyLock;
    static DOC: LazyLock<[Pattern; 2]> = LazyLock::new(|| {
        [
            Pattern::new("#[doc = $TEXT]", SupportLang::Rust),
            Pattern::new("#![doc = $TEXT]", SupportLang::Rust),
        ]
    });
    static SCHEMA: LazyLock<Pattern> = LazyLock::new(|| {
        Pattern::new(
            "#[schemars($$$BEFORE description = $TEXT $$$AFTER)]",
            SupportLang::Rust,
        )
    });
    let mut detected = Vec::new();
    if kinds.contains(&DocumentationKind::DocAttribute) {
        for pattern in DOC.iter() {
            detected.extend(root.find_all(pattern).map(|node| Documentation {
                span: node.range(),
                kind: DocumentationKind::DocAttribute,
                rule_id: "documentation-rust-attribute",
            }));
        }
    }
    if kinds.contains(&DocumentationKind::SchemaDescription) {
        detected.extend(root.find_all(&*SCHEMA).map(|node| Documentation {
            span: node.range(),
            kind: DocumentationKind::SchemaDescription,
            rule_id: "schema-rust-description",
        }));
    }
    detected
}
