use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, ensure};
use rustpython_ast::{
    self as ast, Constant, Expr, ExprContext, Ranged, Stmt, Visitor, text_size::TextRange,
};
use rustpython_parser::Parse;

use super::Documentation;
use crate::{policy::DocumentationKind, syntax::SyntaxNode};

type Names = HashMap<String, Option<String>>;

#[derive(Default)]
struct Bindings(Names);

impl Bindings {
    fn bind(&mut self, name: String, qualified: Option<String>) {
        self.0
            .entry(name)
            .and_modify(|known| {
                if *known != qualified {
                    *known = None;
                }
            })
            .or_insert(qualified);
    }
}

impl Visitor for Bindings {
    fn visit_stmt_import(&mut self, node: ast::StmtImport) {
        for alias in node.names {
            let imported = alias.name.to_string();
            if let Some(name) = alias.asname {
                self.bind(name.to_string(), Some(imported));
            } else {
                let top = imported.split('.').next().unwrap_or_default().to_owned();
                self.bind(top.clone(), Some(top));
            }
        }
    }

    fn visit_stmt_import_from(&mut self, node: ast::StmtImportFrom) {
        let module = node
            .module
            .filter(|_| node.level.is_none_or(|level| level.to_u32() == 0));
        for alias in node.names {
            let qualified = module
                .as_ref()
                .map(|module| format!("{module}.{}", alias.name));
            self.bind(alias.asname.unwrap_or(alias.name).to_string(), qualified);
        }
    }

    fn visit_expr_name(&mut self, node: ast::ExprName) {
        if matches!(node.ctx, ExprContext::Store | ExprContext::Del) {
            self.bind(node.id.to_string(), None);
        }
    }

    fn visit_stmt_function_def(&mut self, node: ast::StmtFunctionDef) {
        self.bind(node.name.to_string(), None);
    }

    fn visit_stmt_async_function_def(&mut self, node: ast::StmtAsyncFunctionDef) {
        self.bind(node.name.to_string(), None);
    }

    fn visit_stmt_class_def(&mut self, node: ast::StmtClassDef) {
        self.bind(node.name.to_string(), None);
    }

    fn visit_expr_lambda(&mut self, _node: ast::ExprLambda) {}
}

struct PythonDocumentation {
    scopes: Vec<Names>,
    detected: Vec<Documentation>,
    kinds: Vec<DocumentationKind>,
}

impl PythonDocumentation {
    fn record(&mut self, range: TextRange, kind: DocumentationKind, rule_id: &'static str) {
        self.detected.push(Documentation {
            span: range.start().to_usize()..range.end().to_usize(),
            kind,
            rule_id,
        });
    }

    fn qualified_name(&self, expression: &Expr) -> Option<String> {
        match expression {
            Expr::Name(name) => self
                .scopes
                .iter()
                .rev()
                .find_map(|scope| scope.get(name.id.as_str()))
                .cloned()
                .flatten(),
            Expr::Attribute(attribute) => Some(format!(
                "{}.{}",
                self.qualified_name(&attribute.value)?,
                attribute.attr
            )),
            _ => None,
        }
    }

    fn visit_body(&mut self, body: Vec<Stmt>, mut bindings: Bindings) {
        if self.kinds.contains(&DocumentationKind::Docstring)
            && let Some(Stmt::Expr(statement)) = body.first()
            && matches!(statement.value.as_ref(), Expr::Constant(value) if matches!(value.value, Constant::Str(_)))
        {
            self.record(
                statement.range(),
                DocumentationKind::Docstring,
                "documentation-python",
            );
        }
        if self.kinds.contains(&DocumentationKind::SchemaDescription) {
            for statement in &body {
                bindings.visit_stmt(statement.clone());
            }
        }
        self.scopes.push(bindings.0);
        for statement in body {
            self.visit_stmt(statement);
        }
        self.scopes.pop();
    }

    fn parameter_bindings(&mut self, arguments: ast::Arguments) -> Bindings {
        let mut bindings = Bindings::default();
        for argument in arguments
            .posonlyargs
            .into_iter()
            .chain(arguments.args)
            .chain(arguments.kwonlyargs)
        {
            bindings.bind(argument.def.arg.to_string(), None);
            if let Some(default) = argument.default {
                self.visit_expr(*default);
            }
        }
        for argument in [arguments.vararg, arguments.kwarg].into_iter().flatten() {
            bindings.bind(argument.arg.to_string(), None);
        }
        bindings
    }

    fn visit_function(&mut self, args: ast::Arguments, decorators: Vec<Expr>, body: Vec<Stmt>) {
        let bindings = self.parameter_bindings(args);
        for decorator in decorators {
            self.visit_expr(decorator);
        }
        self.visit_body(body, bindings);
    }
}

fn is_none(expression: &Expr) -> bool {
    matches!(expression, Expr::Constant(value) if value.value == Constant::None)
}

fn describes_schema(node: &ast::ExprCall, qualified: &str) -> bool {
    let is_field = matches!(qualified, "pydantic.Field" | "pydantic.fields.Field");
    if !is_field
        && !matches!(
            qualified,
            "pydantic.ConfigDict" | "pydantic.config.ConfigDict"
        )
    {
        return false;
    }
    node.keywords.iter().any(|keyword| match keyword.arg.as_ref().map(ast::Identifier::as_str) {
        Some("description") => is_field && !is_none(&keyword.value),
        Some("json_schema_extra") if let Expr::Dict(dictionary) = &keyword.value => {
            dictionary.keys.iter().zip(&dictionary.values).any(|(key, value)| {
                matches!(key, Some(Expr::Constant(key)) if matches!(&key.value, Constant::Str(text) if text == "description"))
                    && !is_none(value)
            })
        }
        _ => false,
    })
}

impl Visitor for PythonDocumentation {
    fn visit_stmt_function_def(&mut self, node: ast::StmtFunctionDef) {
        self.visit_function(*node.args, node.decorator_list, node.body);
    }

    fn visit_stmt_async_function_def(&mut self, node: ast::StmtAsyncFunctionDef) {
        self.visit_function(*node.args, node.decorator_list, node.body);
    }

    fn visit_stmt_class_def(&mut self, node: ast::StmtClassDef) {
        for base in node.bases {
            self.visit_expr(base);
        }
        self.visit_body(node.body, Bindings::default());
    }

    fn visit_expr_lambda(&mut self, node: ast::ExprLambda) {
        let bindings = self.parameter_bindings(*node.args);
        self.scopes.push(bindings.0);
        self.visit_expr(*node.body);
        self.scopes.pop();
    }

    fn visit_expr_call(&mut self, node: ast::ExprCall) {
        if self.kinds.contains(&DocumentationKind::SchemaDescription)
            && self
                .qualified_name(&node.func)
                .is_some_and(|qualified| describes_schema(&node, &qualified))
        {
            self.record(
                node.range(),
                DocumentationKind::SchemaDescription,
                "schema-description-pydantic",
            );
        }
        self.generic_visit_expr_call(node);
    }

    fn visit_keyword(&mut self, node: ast::Keyword) {
        self.visit_expr(node.value);
    }
}

/// Deepest supported Python nesting. One level is one syntax node that rustpython nests
/// inside its parent: a statement inside a compound statement, an expression inside an
/// expression, or a pattern inside a pattern. Deeper files are a tool error, because
/// rustpython clones, visits, and drops its tree recursively.
const MAX_SYNTAX_DEPTH: usize = 1024;

/// Reserved stack for rustpython, so the supported depth does not depend on the platform's
/// main-thread stack. `MAX_SYNTAX_DEPTH` chained `elif` clauses, the deepest-framed syntax,
/// need under 8 MiB in unoptimized builds.
const ANALYSIS_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Tree-sitter wrappers that rustpython stores as a field or list of the enclosing node,
/// so they add no nesting of their own.
const TRANSPARENT_KINDS: &[&str] = &[
    "block",
    "argument_list",
    "parenthesized_expression",
    "case_pattern",
    "pair",
];

/// tree-sitter-python 0.25.0 serializes each open indentation level as two bytes after up
/// to 257 bytes of string state, and aborts the process once that passes 1024 bytes. Open
/// levels have distinct widths, so files with at most this many widths stay in bounds.
const MAX_INDENTATION_WIDTHS: usize = 383;

/// Measures indentation as the tree-sitter-python scanner does: spaces add one, tabs add
/// eight, carriage returns and form feeds reset, and a backslash continuation keeps counting.
pub(super) fn ensure_scannable_indentation(source: &str) -> Result<()> {
    let bytes = source.as_bytes();
    let mut widths = HashSet::new();
    let mut indentation = Some(0_u16);
    let mut line = 0;
    let mut offset = 0;
    while let Some(&byte) = bytes.get(offset) {
        offset += 1;
        match byte {
            b'\n' => {
                line += 1;
                indentation = Some(0);
            }
            b' ' | b'\t' | b'\r' | b'\x0c' => {
                if let Some(width) = &mut indentation {
                    *width = match byte {
                        b' ' => width.wrapping_add(1),
                        b'\t' => width.wrapping_add(8),
                        _ => 0,
                    };
                }
            }
            b'\\' if bytes[offset..].starts_with(b"\n") || bytes[offset..].starts_with(b"\r\n") => {
                offset += if bytes[offset] == b'\r' { 2 } else { 1 };
                line += 1;
            }
            _ => {
                if let Some(width) = indentation.take().filter(|&width| width > 0) {
                    widths.insert(width);
                    ensure!(
                        widths.len() <= MAX_INDENTATION_WIDTHS,
                        "Python indentation at line {} exceeds the nesting depth limit of {MAX_INDENTATION_WIDTHS} distinct indentation widths",
                        line + 1
                    );
                }
            }
        }
    }
    Ok(())
}

fn ensure_supported_nesting(root: &SyntaxNode<'_>) -> Result<()> {
    let mut pending = vec![(root.clone(), 0)];
    while let Some((node, depth)) = pending.pop() {
        ensure!(
            depth <= MAX_SYNTAX_DEPTH,
            "Python syntax at line {} exceeds the nesting depth limit of {MAX_SYNTAX_DEPTH}",
            node.start_pos().line() + 1
        );
        // rustpython nests each `elif` inside the previous clause, and error recovery can
        // leave tokens flat that rustpython nests, so those siblings count as successive levels.
        let chains_every_child = node.is_error();
        let mut sibling_offset = 0;
        for child in node.children() {
            let nests_following = chains_every_child || child.kind() == "elif_clause";
            let level = usize::from(!TRANSPARENT_KINDS.contains(&child.kind().as_ref()));
            pending.push((child, depth + sibling_offset + level));
            if nests_following {
                sibling_offset += 1;
            }
        }
    }
    Ok(())
}

fn analyze(source: &str, kinds: &[DocumentationKind]) -> Result<Vec<Documentation>> {
    let body = ast::Suite::parse(source, "source")?;
    let mut visitor = PythonDocumentation {
        scopes: Vec::new(),
        detected: Vec::new(),
        kinds: kinds.to_vec(),
    };
    visitor.visit_body(body, Bindings::default());
    Ok(visitor.detected)
}

pub(super) fn detect(
    root: &SyntaxNode<'_>,
    source: &str,
    kinds: &[DocumentationKind],
) -> Result<Vec<Documentation>> {
    if !kinds.iter().any(|kind| {
        matches!(
            kind,
            DocumentationKind::Docstring | DocumentationKind::SchemaDescription
        )
    }) {
        return Ok(Vec::new());
    }
    ensure_supported_nesting(root)?;
    std::thread::scope(|scope| {
        let analysis = std::thread::Builder::new()
            .name("python-analysis".into())
            .stack_size(ANALYSIS_STACK_BYTES)
            .spawn_scoped(scope, || analyze(source, kinds))
            .context("starting Python analysis")?;
        analysis
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}
