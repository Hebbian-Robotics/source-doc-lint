use std::{collections::HashMap, thread};

use anyhow::{Result, anyhow, ensure};
use rustpython_ast::{
    self as ast, Constant, Expr, ExprContext, Ranged, Stmt, Visitor,
    text_size::{TextRange, TextSize},
};
use rustpython_parser::{Parse, Tok, lexer::LexResult};

use super::Documentation;
use crate::policy::DocumentationKind;

/// Parsing, walking, cloning, and dropping a rustpython syntax tree all recurse
/// once per level, so deeper input would exhaust the native stack.
const MAX_PYTHON_NESTING: usize = 4096;

/// A tree at the nesting limit needs about 20 MiB of stack in debug builds,
/// more than the main thread has on any supported platform.
const PYTHON_CHECK_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Tree levels a bracket pair can add beyond the tokens inside it, such as an
/// unparenthesized tuple and the statement or `yield` that holds it.
const BRACKET_NESTING_SLACK: usize = 4;

/// Tree levels a logical line can add beyond its tokens, such as a one-line
/// compound statement, its simple statement, and an expression wrapper.
const LOGICAL_LINE_NESTING_SLACK: usize = 8;

/// Tree levels an f-string adds around a placeholder expression, including one
/// nested format specification.
const FORMATTED_STRING_NESTING_SLACK: usize = 8;

type Names = HashMap<String, Option<String>>;

#[derive(Default)]
struct NestingGuard {
    depth: usize,
    exceeded: bool,
}

impl NestingGuard {
    fn enter(&mut self) -> bool {
        if self.depth >= MAX_PYTHON_NESTING {
            self.exceeded = true;
            return false;
        }
        self.depth += 1;
        true
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }
}

#[derive(Default)]
struct Bindings {
    names: Names,
    nesting: NestingGuard,
}

impl Bindings {
    fn bind(&mut self, name: String, qualified: Option<String>) {
        self.names
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
    fn visit_stmt(&mut self, node: Stmt) {
        if self.nesting.enter() {
            self.generic_visit_stmt(node);
            self.nesting.leave();
        }
    }

    fn visit_expr(&mut self, node: Expr) {
        if self.nesting.enter() {
            self.generic_visit_expr(node);
            self.nesting.leave();
        }
    }

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
    nesting: NestingGuard,
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
            self.nesting.exceeded |= bindings.nesting.exceeded;
        }
        self.scopes.push(bindings.names);
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

#[derive(Default)]
struct BracketFrame {
    segment_tokens: usize,
    deepest_segment_child: usize,
    deepest_segment: usize,
    lambdas_awaiting_colon: usize,
    previous_string_nesting: Option<usize>,
}

impl BracketFrame {
    fn count(&mut self, nesting: usize) {
        self.segment_tokens += nesting;
        self.previous_string_nesting = None;
    }

    /// Adjacent string literals become one node, so the run is as deep as its
    /// deepest part.
    fn count_string(&mut self, nesting: usize) {
        let previous = self.previous_string_nesting.unwrap_or(0);
        self.segment_tokens += nesting.saturating_sub(previous);
        self.previous_string_nesting = Some(nesting.max(previous));
    }

    fn end_segment(&mut self) {
        self.deepest_segment = self
            .deepest_segment
            .max(self.segment_tokens + self.deepest_segment_child);
        self.segment_tokens = 0;
        self.deepest_segment_child = 0;
        self.previous_string_nesting = None;
    }

    fn nesting(mut self) -> usize {
        self.end_segment();
        self.deepest_segment
    }
}

/// Upper bound on the depth of the tree rustpython builds from these tokens,
/// including partial trees it drops after a syntax error. Each tree level
/// consumes a token of its own except for a bounded number per bracket pair
/// and per logical line. Comma-separated items are siblings, except inside
/// lambda parameters, so a long flat literal stays shallow.
fn python_nesting_bound(tokens: &[LexResult]) -> usize {
    let mut deepest = 0;
    let mut frames = vec![BracketFrame::default()];
    let mut elif_chains_by_block = vec![0_usize];
    let mut at_logical_line_start = true;
    for (token, _) in tokens.iter().flatten() {
        match token {
            Tok::Indent => {
                elif_chains_by_block.push(0);
                continue;
            }
            Tok::Dedent => {
                if elif_chains_by_block.len() > 1 {
                    elif_chains_by_block.pop();
                }
                continue;
            }
            Tok::Newline | Tok::EndOfFile => {
                deepest = deepest.max(end_logical_line(&mut frames, &elif_chains_by_block));
                at_logical_line_start = true;
                continue;
            }
            _ => {}
        }
        if at_logical_line_start {
            at_logical_line_start = false;
            if let Some(chain) = elif_chains_by_block.last_mut() {
                match token {
                    Tok::Elif => *chain += 1,
                    Tok::Else => {}
                    _ => *chain = 0,
                }
            }
        }
        let Some(frame) = frames.last_mut() else {
            continue;
        };
        match token {
            Tok::Lpar | Tok::Lsqb | Tok::Lbrace => {
                frame.count(1);
                frames.push(BracketFrame::default());
            }
            Tok::Rpar | Tok::Rsqb | Tok::Rbrace => close_bracket_frame(&mut frames),
            Tok::Comma | Tok::Semi if frame.lambdas_awaiting_colon == 0 => frame.end_segment(),
            Tok::Lambda => {
                frame.lambdas_awaiting_colon += 1;
                frame.count(1);
            }
            Tok::Colon => {
                frame.lambdas_awaiting_colon = frame.lambdas_awaiting_colon.saturating_sub(1);
                frame.count(1);
            }
            Tok::String { value, kind, .. } if kind.is_any_fstring() => {
                frame.count_string(formatted_string_nesting_bound(value));
            }
            Tok::String { .. } => frame.count_string(1),
            _ => frame.count(1),
        }
    }
    deepest.max(end_logical_line(&mut frames, &elif_chains_by_block))
}

fn close_bracket_frame(frames: &mut Vec<BracketFrame>) {
    if frames.len() < 2 {
        return;
    }
    let Some(closed) = frames.pop() else {
        return;
    };
    let closed_nesting = closed.nesting() + BRACKET_NESTING_SLACK;
    if let Some(parent) = frames.last_mut() {
        parent.deepest_segment_child = parent.deepest_segment_child.max(closed_nesting);
        parent.previous_string_nesting = None;
    }
}

/// Each block adds its compound statement and, for `except` and `case`, the
/// handler or case node. Each `elif` nests its `if` inside the previous one.
fn end_logical_line(frames: &mut Vec<BracketFrame>, elif_chains_by_block: &[usize]) -> usize {
    while frames.len() > 1 {
        close_bracket_frame(frames);
    }
    let line_nesting = std::mem::take(&mut frames[0]).nesting();
    let block_nesting =
        2 * (elif_chains_by_block.len() - 1) + elif_chains_by_block.iter().sum::<usize>();
    block_nesting + LOGICAL_LINE_NESTING_SLACK + line_nesting
}

/// rustpython parses each f-string placeholder as its own expression. This
/// scan ends a placeholder no earlier than rustpython does, so the longest
/// placeholder bounds the deepest expression inside the string.
fn formatted_string_nesting_bound(content: &str) -> usize {
    let mut longest_placeholder = 0;
    let mut characters = content.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '{' {
            continue;
        }
        if characters.peek() == Some(&'{') {
            characters.next();
            continue;
        }
        let mut placeholder_length = 1;
        let mut open_delimiters = vec!['{'];
        while let Some(character) = characters.next() {
            placeholder_length += 1;
            match character {
                '(' | '[' | '{' => open_delimiters.push(character),
                ')' | ']' | '}' => {
                    let opening = match character {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    if open_delimiters.last() == Some(&opening) {
                        open_delimiters.pop();
                    }
                    if open_delimiters.is_empty() {
                        break;
                    }
                }
                '\'' | '"' => {
                    for quoted in characters.by_ref() {
                        placeholder_length += 1;
                        if quoted == character {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        longest_placeholder = longest_placeholder.max(placeholder_length);
    }
    FORMATTED_STRING_NESTING_SLACK + 2 * longest_placeholder
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
    fn visit_stmt(&mut self, node: Stmt) {
        if self.nesting.enter() {
            self.generic_visit_stmt(node);
            self.nesting.leave();
        }
    }

    fn visit_expr(&mut self, node: Expr) {
        if self.nesting.enter() {
            self.generic_visit_expr(node);
            self.nesting.leave();
        }
    }

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
        self.scopes.push(bindings.names);
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
    thread::scope(|scope| {
        thread::Builder::new()
            .name("python-documentation".to_owned())
            .stack_size(PYTHON_CHECK_STACK_BYTES)
            .spawn_scoped(scope, || detect_within_nesting_limit(source, kinds))?
            .join()
            .map_err(|_| anyhow!("the Python documentation check panicked"))?
    })
}

fn detect_within_nesting_limit(
    source: &str,
    kinds: &[DocumentationKind],
) -> Result<Vec<Documentation>> {
    let tokens: Vec<LexResult> = ast::Suite::lex_starts_at(source, TextSize::default()).collect();
    ensure_nesting_within_limit(python_nesting_bound(&tokens) <= MAX_PYTHON_NESTING)?;
    let body = ast::Suite::parse_tokens(tokens, "source")?;
    let mut visitor = PythonDocumentation {
        scopes: Vec::new(),
        detected: Vec::new(),
        kinds: kinds.to_vec(),
        nesting: NestingGuard::default(),
    };
    visitor.visit_body(body, Bindings::default());
    ensure_nesting_within_limit(!visitor.nesting.exceeded)?;
    Ok(visitor.detected)
}

fn ensure_nesting_within_limit(within_limit: bool) -> Result<()> {
    ensure!(
        within_limit,
        "Python nesting is too deep to check; the limit is {MAX_PYTHON_NESTING} levels"
    );
    Ok(())
}
