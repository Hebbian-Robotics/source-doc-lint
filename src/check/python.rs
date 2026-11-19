use std::collections::HashMap;

use anyhow::Result;
use rustpython_ast::{
    self as ast, Constant, Expr, ExprContext, Ranged, Stmt, Visitor, text_size::TextRange,
};
use rustpython_parser::Parse;

use super::Documentation;
use crate::policy::DocumentationKind;

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

pub(super) fn detect(source: &str, kinds: &[DocumentationKind]) -> Result<Vec<Documentation>> {
    if !kinds.iter().any(|kind| {
        matches!(
            kind,
            DocumentationKind::Docstring | DocumentationKind::SchemaDescription
        )
    }) {
        return Ok(Vec::new());
    }
    let body = ast::Suite::parse(source, "source")?;
    let mut visitor = PythonDocumentation {
        scopes: Vec::new(),
        detected: Vec::new(),
        kinds: kinds.to_vec(),
    };
    visitor.visit_body(body, Bindings::default());
    Ok(visitor.detected)
}
