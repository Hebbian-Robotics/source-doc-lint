use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

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
        for binding in clause.children() {
            match binding.kind().as_ref() {
                "identifier" => {
                    self.namespaces.insert(binding.text().into_owned());
                }
                "namespace_import" => {
                    if let Some(name) = binding.children().find(|node| node.kind() == "identifier")
                    {
                        self.namespaces.insert(name.text().into_owned());
                    }
                }
                "named_imports" => {
                    for specifier in binding
                        .children()
                        .filter(|node| node.kind() == "import_specifier")
                    {
                        self.register_specifier(&specifier);
                    }
                }
                _ => {}
            }
        }
    }

    fn register_specifier(&mut self, specifier: &SyntaxNode<'_>) {
        let Some(name) = specifier.field("name") else {
            return;
        };
        let local = specifier
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

    fn next_receiver<'source>(&self, expression: &SyntaxNode<'source>) -> ReceiverStep<'source> {
        match expression.kind().as_ref() {
            "identifier" => ReceiverStep::Root(ReceiverRoot::Alias(expression.text().into())),
            "member_expression" => expression.field("object").map_or(
                ReceiverStep::Root(ReceiverRoot::Known(false)),
                ReceiverStep::Descend,
            ),
            "call_expression" => {
                let Some(function) = expression.field("function") else {
                    return ReceiverStep::Root(ReceiverRoot::Known(false));
                };
                match function.kind().as_ref() {
                    "identifier" => ReceiverStep::Root(ReceiverRoot::Known(
                        self.factories.contains(function.text().as_ref()),
                    )),
                    "member_expression" => match function.field("object") {
                        Some(object) if object.kind() == "identifier" => {
                            let name = object.text();
                            let builds_schema = self.namespaces.contains(name.as_ref())
                                && function.field("property").is_some_and(|property| {
                                    FACTORIES.contains(&property.text().as_ref())
                                });
                            ReceiverStep::Root(if builds_schema {
                                ReceiverRoot::Known(true)
                            } else {
                                ReceiverRoot::Alias(name.into())
                            })
                        }
                        Some(object) => ReceiverStep::Descend(object),
                        None => ReceiverStep::Root(ReceiverRoot::Known(false)),
                    },
                    _ => ReceiverStep::Root(ReceiverRoot::Known(false)),
                }
            }
            _ => ReceiverStep::Root(ReceiverRoot::Known(false)),
        }
    }
}

/// Where a member and call chain bottoms out: a fixed answer, or a name that is a schema
/// exactly when it is bound to one.
#[derive(Clone)]
enum ReceiverRoot {
    Known(bool),
    Alias(Rc<str>),
}

enum ReceiverStep<'source> {
    Root(ReceiverRoot),
    Descend(SyntaxNode<'source>),
}

/// Resolves receiver chains without recursion and remembers every receiver on the way, so
/// nested and overlapping chains are walked once in total.
struct ReceiverRoots<'bindings> {
    bindings: &'bindings ZodBindings,
    resolved: HashMap<usize, ReceiverRoot>,
}

impl ReceiverRoots<'_> {
    fn resolve(&mut self, expression: &SyntaxNode<'_>) -> ReceiverRoot {
        let mut chain = Vec::new();
        let mut receiver = expression.clone();
        let root = loop {
            if let Some(root) = self.resolved.get(&receiver.node_id()) {
                break root.clone();
            }
            match self.bindings.next_receiver(&receiver) {
                ReceiverStep::Root(root) => break root,
                ReceiverStep::Descend(next) => {
                    chain.push(receiver.node_id());
                    receiver = next;
                }
            }
        };
        for node_id in chain {
            self.resolved.insert(node_id, root.clone());
        }
        root
    }
}

fn is_schema(root: &ReceiverRoot, schemas: &HashSet<Rc<str>>) -> bool {
    match root {
        ReceiverRoot::Known(known) => *known,
        ReceiverRoot::Alias(name) => schemas.contains(name),
    }
}

/// A local schema alias is any name bound to a recognized receiver, including through
/// other aliases in any declaration order.
fn schema_aliases(
    roots: &mut ReceiverRoots<'_>,
    candidates: &[(Rc<str>, SyntaxNode<'_>)],
) -> HashSet<Rc<str>> {
    let mut schemas = HashSet::new();
    let mut newly_recognized = Vec::new();
    let mut aliases_of: HashMap<Rc<str>, Vec<Rc<str>>> = HashMap::new();
    for (name, value) in candidates {
        match roots.resolve(value) {
            ReceiverRoot::Known(true) => newly_recognized.push(Rc::clone(name)),
            ReceiverRoot::Known(false) => {}
            ReceiverRoot::Alias(target) => {
                aliases_of.entry(target).or_default().push(Rc::clone(name));
            }
        }
    }
    while let Some(schema) = newly_recognized.pop() {
        if schemas.insert(Rc::clone(&schema)) {
            newly_recognized.extend(aliases_of.remove(&schema).into_iter().flatten());
        }
    }
    schemas
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

fn opens_scope(node: &SyntaxNode<'_>) -> bool {
    matches!(
        node.kind().as_ref(),
        "statement_block" | "arrow_function" | "switch_body" | "for_statement" | "for_in_statement"
    )
}

struct MetadataCall<'source> {
    call: SyntaxNode<'source>,
    scope_start: usize,
}

fn schema_description(
    roots: &mut ReceiverRoots<'_>,
    schemas: &HashSet<Rc<str>>,
    metadata: &HashMap<(usize, String), usize>,
    MetadataCall { call, scope_start }: &MetadataCall<'_>,
) -> Option<Documentation> {
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
                            .get(&(*scope_start, value.text().into_owned()))
                            .is_some_and(|&offset| offset < call.range().start)))
        }
        _ => false,
    };
    (matched && is_schema(&roots.resolve(&function.field("object")?), schemas)).then(|| {
        Documentation {
            span: call.range(),
            kind: DocumentationKind::SchemaDescription,
            rule_id: "schema-description-zod",
        }
    })
}

pub(super) fn detect(root: &SyntaxNode<'_>) -> Vec<Documentation> {
    let mut bindings = ZodBindings::default();
    let mut schema_candidates = Vec::new();
    let mut metadata = HashMap::new();
    let mut calls = Vec::new();
    // Each entry carries the start of its enclosing scope and whether its parent declares
    // `const`, because looking ancestors up from a node costs time proportional to its depth.
    let mut pending = vec![(root.clone(), 0, false)];
    while let Some((node, scope_start, declared_const)) = pending.pop() {
        match node.kind().as_ref() {
            "import_statement" => bindings.register_import(&node),
            "variable_declarator" => {
                if let (Some(name), Some(value)) = (node.field("name"), node.field("value"))
                    && name.kind() == "identifier"
                {
                    if declared_const && contains_description(&value) {
                        metadata
                            .insert((scope_start, name.text().into_owned()), node.range().start);
                    }
                    schema_candidates.push((Rc::from(name.text().as_ref()), value));
                }
            }
            "call_expression" => calls.push(MetadataCall {
                call: node.clone(),
                scope_start,
            }),
            _ => {}
        }
        let child_scope_start = if opens_scope(&node) {
            node.range().start
        } else {
            scope_start
        };
        let children: Vec<_> = node.children().collect();
        let declares_const = children.iter().any(|child| child.kind() == "const");
        pending.extend(
            children
                .into_iter()
                .rev()
                .map(|child| (child, child_scope_start, declares_const)),
        );
    }
    let mut roots = ReceiverRoots {
        bindings: &bindings,
        resolved: HashMap::new(),
    };
    let schemas = schema_aliases(&mut roots, &schema_candidates);
    calls
        .iter()
        .filter_map(|call| schema_description(&mut roots, &schemas, &metadata, call))
        .collect()
}
