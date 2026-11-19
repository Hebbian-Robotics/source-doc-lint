use std::collections::{HashMap, HashSet};

use super::Documentation;
use crate::{policy::DocumentationKind, syntax::SyntaxNode};

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

    fn recognizes(&self, expression: &SyntaxNode<'_>) -> bool {
        match expression.kind().as_ref() {
            "identifier" => self.schemas.contains(expression.text().as_ref()),
            "member_expression" => expression
                .field("object")
                .is_some_and(|object| self.recognizes(&object)),
            "call_expression" => {
                expression.field("function").is_some_and(|function| {
                    match function.kind().as_ref() {
                        "identifier" => self.factories.contains(function.text().as_ref()),
                        "member_expression" => function.field("object").is_some_and(|object| {
                            self.recognizes(&object)
                                || (object.kind() == "identifier"
                                    && self.namespaces.contains(object.text().as_ref())
                                    && function.field("property").is_some_and(|property| {
                                        FACTORIES.contains(&property.text().as_ref())
                                    }))
                        }),
                        _ => false,
                    }
                })
            }
            _ => false,
        }
    }
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

pub(super) fn detect(root: &SyntaxNode<'_>) -> Vec<Documentation> {
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
            if !bindings.schemas.contains(name) && bindings.recognizes(value) {
                bindings.schemas.insert(name.clone());
            }
        }
        if previous_count == bindings.schemas.len() {
            break;
        }
    }
    calls
        .into_iter()
        .filter_map(|call| {
            let function = call.field("function")?;
            let method = function.field("property")?;
            let matched = match method.text().as_ref() {
                "describe" => true,
                "meta" => {
                    let arguments = call.field("arguments")?;
                    let mut values = arguments
                        .children()
                        .filter(|node| node.is_named() && node.kind() != "comment");
                    let value = values.next()?;
                    values.next().is_none()
                        && (contains_description(&value)
                            || (value.kind() == "identifier"
                                && metadata
                                    .get(&(scope_start(&call), value.text().into_owned()))
                                    .is_some_and(|&offset| offset < call.range().start)))
                }
                _ => false,
            };
            (matched && bindings.recognizes(&function.field("object")?)).then(|| Documentation {
                span: call.range(),
                kind: DocumentationKind::SchemaDescription,
                rule_id: "schema-description-zod",
            })
        })
        .collect()
}
