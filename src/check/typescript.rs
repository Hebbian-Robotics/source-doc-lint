use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};

use super::Documentation;
use crate::{policy::DocumentationKind, syntax::SyntaxNode};

/// Zod chains stay far shorter than this. Longer receiver chains are rejected so a
/// hostile initializer cannot make schema detection unbounded.
const MAX_RECEIVER_DEPTH: usize = 256;

const FACTORIES: &[&str] = &[
    "string",
    "number",
    "bigint",
    "boolean",
    "date",
    "symbol",
    "undefined",
    "null",
    "any",
    "unknown",
    "never",
    "void",
    "array",
    "object",
    "strictObject",
    "looseObject",
    "union",
    "discriminatedUnion",
    "intersection",
    "tuple",
    "record",
    "partialRecord",
    "map",
    "set",
    "enum",
    "nativeEnum",
    "literal",
    "literals",
    "lazy",
    "promise",
    "function",
    "custom",
    "templateLiteral",
    "file",
    "instanceof",
    "coerce",
];

#[derive(Default)]
struct ZodBindings {
    namespaces: HashSet<String>,
    factories: HashSet<String>,
    schemas: HashSet<String>,
}

impl ZodBindings {
    fn register_import(&mut self, declaration: &SyntaxNode<'_>) {
        let Some(source) = declaration.field("source") else {
            return;
        };
        if !matches!(
            source.text().trim_matches(['\'', '"']),
            "zod" | "zod/v3" | "zod/v4"
        ) {
            return;
        }
        let Some(clause) = declaration
            .children()
            .find(|node| node.kind() == "import_clause")
        else {
            return;
        };
        for binding in clause.dfs() {
            match binding.kind().as_ref() {
                "namespace_import" => {
                    if let Some(name) = binding.children().find(|node| node.kind() == "identifier")
                    {
                        self.namespaces.insert(name.text().into_owned());
                    }
                }
                "identifier"
                    if binding
                        .parent()
                        .is_some_and(|parent| parent.kind() == "import_clause") =>
                {
                    self.namespaces.insert(binding.text().into_owned());
                }
                "import_specifier" => {
                    let Some(name) = binding.field("name") else {
                        continue;
                    };
                    let local = binding
                        .field("alias")
                        .unwrap_or_else(|| name.clone())
                        .text()
                        .into_owned();
                    if name.text() == "z" {
                        self.namespaces.insert(local);
                    } else if FACTORIES.contains(&name.text().as_ref()) {
                        self.factories.insert(local);
                    }
                }
                _ => {}
            }
        }
    }

    fn recognizes(&self, expression: &SyntaxNode<'_>) -> Result<bool> {
        let mut receiver = expression.clone();
        for _ in 0..=MAX_RECEIVER_DEPTH {
            match self.next_receiver(&receiver) {
                ReceiverStep::Resolved(recognized) => return Ok(recognized),
                ReceiverStep::Descend(next) => receiver = next,
            }
        }
        bail!(
            "schema receiver chain at line {} exceeds the receiver depth limit of {MAX_RECEIVER_DEPTH}",
            expression.start_pos().line() + 1
        )
    }

    fn next_receiver<'source>(&self, expression: &SyntaxNode<'source>) -> ReceiverStep<'source> {
        match expression.kind().as_ref() {
            "identifier" => {
                ReceiverStep::Resolved(self.schemas.contains(expression.text().as_ref()))
            }
            "member_expression" => expression
                .field("object")
                .map_or(ReceiverStep::Resolved(false), ReceiverStep::Descend),
            "call_expression" => {
                let Some(function) = expression.field("function") else {
                    return ReceiverStep::Resolved(false);
                };
                match function.kind().as_ref() {
                    "identifier" => {
                        ReceiverStep::Resolved(self.factories.contains(function.text().as_ref()))
                    }
                    "member_expression" => match function.field("object") {
                        Some(object) if object.kind() == "identifier" => {
                            let name = object.text();
                            ReceiverStep::Resolved(
                                self.schemas.contains(name.as_ref())
                                    || (self.namespaces.contains(name.as_ref())
                                        && function.field("property").is_some_and(|property| {
                                            FACTORIES.contains(&property.text().as_ref())
                                        })),
                            )
                        }
                        Some(object) => ReceiverStep::Descend(object),
                        None => ReceiverStep::Resolved(false),
                    },
                    _ => ReceiverStep::Resolved(false),
                }
            }
            _ => ReceiverStep::Resolved(false),
        }
    }
}

enum ReceiverStep<'source> {
    Resolved(bool),
    Descend(SyntaxNode<'source>),
}

fn contains_description(node: &SyntaxNode<'_>) -> bool {
    node.kind() == "object"
        && node.children().any(|pair| {
            pair.kind() == "pair"
                && pair.field("key").is_some_and(|key| {
                    matches!(
                        key.text().as_ref(),
                        "description" | "'description'" | "\"description\""
                    )
                })
        })
}

fn scope_start(node: &SyntaxNode<'_>) -> usize {
    node.ancestors()
        .find(|ancestor| {
            matches!(
                ancestor.kind().as_ref(),
                "statement_block"
                    | "arrow_function"
                    | "switch_body"
                    | "for_statement"
                    | "for_in_statement"
            )
        })
        .map_or(0, |scope| scope.range().start)
}

fn schema_description(
    bindings: &ZodBindings,
    metadata: &HashMap<(usize, String), usize>,
    call: &SyntaxNode<'_>,
) -> Result<Option<Documentation>> {
    let Some(function) = call.field("function") else {
        return Ok(None);
    };
    let Some(method) = function.field("property") else {
        return Ok(None);
    };
    let matched = match method.text().as_ref() {
        "describe" => true,
        "meta" => call.field("arguments").is_some_and(|arguments| {
            let mut values = arguments
                .children()
                .filter(|node| node.is_named() && node.kind() != "comment");
            values.next().is_some_and(|value| {
                values.next().is_none()
                    && (contains_description(&value)
                        || (value.kind() == "identifier"
                            && metadata
                                .get(&(scope_start(call), value.text().into_owned()))
                                .is_some_and(|&offset| offset < call.range().start)))
            })
        }),
        _ => false,
    };
    let Some(receiver) = function.field("object").filter(|_| matched) else {
        return Ok(None);
    };
    Ok(bindings.recognizes(&receiver)?.then(|| Documentation {
        span: call.range(),
        kind: DocumentationKind::SchemaDescription,
        rule_id: "schema-description-zod",
    }))
}

pub(super) fn detect(root: &SyntaxNode<'_>) -> Result<Vec<Documentation>> {
    let mut bindings = ZodBindings::default();
    let mut schema_candidates = Vec::new();
    let mut metadata = HashMap::new();
    let mut calls = Vec::new();
    for node in root.dfs() {
        match node.kind().as_ref() {
            "import_statement" => bindings.register_import(&node),
            "variable_declarator" => {
                let (Some(name), Some(value)) = (node.field("name"), node.field("value")) else {
                    continue;
                };
                if name.kind() != "identifier" {
                    continue;
                }
                if contains_description(&value)
                    && node.parent().is_some_and(|parent| {
                        parent.children().any(|child| child.kind() == "const")
                    })
                {
                    metadata.insert(
                        (scope_start(&node), name.text().into_owned()),
                        node.range().start,
                    );
                }
                schema_candidates.push((name.text().into_owned(), value));
            }
            "call_expression" => calls.push(node),
            _ => {}
        }
    }
    loop {
        let previous_count = bindings.schemas.len();
        for (name, value) in &schema_candidates {
            if !bindings.schemas.contains(name) && bindings.recognizes(value)? {
                bindings.schemas.insert(name.clone());
            }
        }
        if previous_count == bindings.schemas.len() {
            break;
        }
    }
    calls
        .iter()
        .filter_map(|call| schema_description(&bindings, &metadata, call).transpose())
        .collect()
}
