use ast_grep_core::{Node, tree_sitter::StrDoc};
use ast_grep_language::SupportLang;
use regex::Regex;
use serde::Serialize;
use std::{path::Path, sync::LazyLock};

pub type SyntaxNode<'source> = Node<'source, StrDoc<SupportLang>>;

#[derive(Clone, Copy, Serialize)]
pub enum Language {
    Python,
    Rust,
    TypeScript,
    #[serde(rename = "Tsx")]
    Tsx,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "py" | "pyi" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            _ => None,
        }
    }

    pub fn parser(self) -> SupportLang {
        match self {
            Self::Python => SupportLang::Python,
            Self::Rust => SupportLang::Rust,
            Self::TypeScript => SupportLang::TypeScript,
            Self::Tsx => SupportLang::Tsx,
        }
    }

    pub fn rule_suffix(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
        }
    }

    #[expect(
        clippy::expect_used,
        reason = "The directive patterns are fixed literals owned by the CLI."
    )]
    pub fn is_directive(self, comment: &str) -> bool {
        static PYTHON: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^#\s*(!|noqa\b|type:\s*ignore\b|ty:\s*ignore\b|ruff:|fmt:|pragma:|coding[:=]|SPDX-License-Identifier:|Copyright\b|copyright\b)").expect("valid Python directive pattern")
        });
        static RUST: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^(?://|/\*)\s*(SPDX-License-Identifier:|Copyright\b|copyright\b)")
                .expect("valid Rust directive pattern")
        });
        static TYPESCRIPT: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^(?://|/\*)\s*(biome-ignore|eslint-|@ts-|istanbul|c8|v8|SPDX-License-Identifier:|Copyright\b|copyright\b)").expect("valid TypeScript directive pattern")
        });
        match self {
            Self::Python => PYTHON.is_match(comment),
            Self::Rust => RUST.is_match(comment),
            Self::TypeScript | Self::Tsx => TYPESCRIPT.is_match(comment),
        }
    }

    pub fn is_doc_comment(self, comment: &str) -> bool {
        match self {
            Self::Python => false,
            Self::Rust => {
                comment.starts_with("//!")
                    || (comment.starts_with("///") && !comment.starts_with("////"))
                    || comment.starts_with("/*!")
                    || (comment.starts_with("/**")
                        && !comment.starts_with("/***")
                        && comment != "/**/")
            }
            Self::TypeScript | Self::Tsx => {
                comment.starts_with("/**") && !comment.starts_with("/***")
            }
        }
    }
}

pub fn is_comment(node: &SyntaxNode<'_>) -> bool {
    matches!(
        node.kind().as_ref(),
        "comment" | "line_comment" | "block_comment"
    )
}

pub fn is_ordinary_comment(language: Language, node: &SyntaxNode<'_>) -> bool {
    if !is_comment(node) {
        return false;
    }
    let comment = node.text();
    match language {
        Language::Python => true,
        Language::Rust => !language.is_doc_comment(&comment) && !comment.starts_with("/***"),
        Language::TypeScript | Language::Tsx => !comment.starts_with("/**"),
    }
}
