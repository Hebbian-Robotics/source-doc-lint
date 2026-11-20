//! Python files outside a public-interface directory are parsed by tree-sitter and then
//! parsed and walked by `RustPython`, whose visitors recurse. Ordinary nesting must still
//! scan, and nesting deep enough to overflow an 8 MiB main stack, or to overrun the
//! tree-sitter Python scanner's indentation state, must come back as the nesting-depth
//! tool error (exit 2 and a JSON `error` naming the file) instead of aborting the process.
//!
//! The stack size is pinned with `ulimit` on Unix so a larger ambient limit
//! cannot hide the abort. Depths below were observed to abort on that stack
//! while the `RustPython` 0.4 parser still accepted the file.
//!
//! Not covered, because this visitor does not recurse there (rustpython-ast
//! 0.4.0 `src/gen/visitor.rs`): comprehension iterators and targets
//! (`generic_visit_comprehension` is empty), match-case bodies
//! (`generic_visit_match_case` is empty), and argument annotations
//! (`generic_visit_arguments` is empty; parameter defaults are visited and are
//! covered below). Nested f-strings are rejected by the parser as a tool
//! error before the walk. Parentheses do not build nested as-patterns.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "The nesting tests fail immediately when fixture setup or the scanner report is unusable."
)]

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use serde::Deserialize;
use tempfile::tempdir;

const SAFE_DEPTH: usize = 256;
const DEEP_EXPR: usize = 20_000;
const DEEP_STMT: usize = 4_000;
const DEEP_PATTERN: usize = 100_000;
/// Enough one-space indentation levels to overrun tree-sitter-python 0.25.0's 1024-byte
/// scanner state while a string is open.
const DEEP_INDENT: usize = 600;

#[derive(Debug, Deserialize)]
struct Report {
    error: Option<String>,
    findings: Vec<serde_json::Value>,
}

fn public_interface(kinds: Option<&str>) -> String {
    let mut config = String::from("[public_interface]\ndirectories = [\"sdk\"]\n");
    if let Some(kinds) = kinds {
        config.push_str("kinds = [");
        config.push_str(kinds);
        config.push_str("]\n");
    }
    config
}

fn scan(root: &Path, config: &str, file_name: &str, source: &str) -> Output {
    fs::write(root.join("source-doc-lint.toml"), config).unwrap();
    fs::write(root.join(file_name), source).unwrap();
    let binary = env!("CARGO_BIN_EXE_source-doc-lint");
    let mut command = if cfg!(unix) {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("ulimit -s 8192 && exec \"$@\"")
            .arg("sh")
            .arg(binary);
        command
    } else {
        Command::new(binary)
    };
    command
        .current_dir(root)
        .arg("--json")
        .arg(file_name)
        .output()
        .expect("run scanner")
}

fn tool_error_failure(name: &str, output: &Output) -> Option<String> {
    if output.status.code() != Some(2) {
        return Some(format!(
            "{name}: status {:?} stderr {} stdout {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    let report: Report = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{name}: JSON report: {error}"));
    match report.error.as_deref() {
        Some(error) if error.contains("nesting depth limit") && error.contains(name) => None,
        _ => Some(format!(
            "{name}: expected the nesting depth error naming the file, got {report:?}"
        )),
    }
}

fn assert_tool_errors(kinds: Option<&str>, cases: &[(&str, String)]) {
    let root = tempdir().unwrap();
    let config = public_interface(kinds);
    let mut failures = Vec::new();
    for (name, source) in cases {
        let output = scan(root.path(), &config, name, source);
        if let Some(failure) = tool_error_failure(name, &output) {
            failures.push(failure);
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

fn wrapped(prefix: &str, depth: usize, middle: &str, suffix: &str) -> String {
    let mut source =
        String::with_capacity(prefix.len() * depth + middle.len() + suffix.len() * depth);
    for _ in 0..depth {
        source.push_str(prefix);
    }
    source.push_str(middle);
    for _ in 0..depth {
        source.push_str(suffix);
    }
    source.push('\n');
    source
}

fn attribute_chain(depth: usize) -> String {
    let mut source = String::from("a");
    for index in 0..depth {
        source.push_str(".a");
        source.push_str(&index.to_string());
    }
    source
}

fn indented(header: &str, depth: usize) -> String {
    let mut source = String::new();
    for level in 0..depth {
        for _ in 0..level {
            source.push(' ');
        }
        source.push_str(header);
        source.push('\n');
    }
    for _ in 0..depth {
        source.push(' ');
    }
    source.push_str("pass\n");
    source
}

fn indented_with_open_string(depth: usize) -> String {
    let mut source = indented("def f():", depth);
    source.truncate(source.len() - "pass\n".len());
    source.push_str("x = f\"{'text'}\"\n");
    source
}

fn nested_handler(handler: &str, depth: usize) -> String {
    let mut source = String::new();
    for level in 0..depth {
        let indent = " ".repeat(level);
        source.push_str(&indent);
        source.push_str("try:\n");
        source.push_str(&indent);
        source.push_str(" pass\n");
        source.push_str(&indent);
        source.push_str(handler);
        source.push('\n');
    }
    for _ in 0..depth {
        source.push(' ');
    }
    source.push_str("pass\n");
    source
}

fn expression_cases(depth: usize) -> Vec<(&'static str, String)> {
    vec![
        ("list.py", wrapped("[", depth, "0", "]")),
        ("call.py", wrapped("f(", depth, "0", ")")),
        ("lambda.py", wrapped("lambda: ", depth, "0", "")),
        ("tuple.py", wrapped("((", depth, "0", ",),)")),
        ("subscript.py", wrapped("x[", depth, "0", "]")),
        ("slice.py", wrapped("x[", depth, "0:0", "]")),
        ("binop.py", wrapped("(0+", depth, "0", ")")),
        ("unary.py", wrapped("+", depth, "0", "")),
        ("boolop.py", wrapped("(0 and ", depth, "0", ")")),
        ("ifexp.py", wrapped("(0 if 1 else ", depth, "0", ")")),
        ("dict.py", wrapped("{0: ", depth, "0", "}")),
        ("set.py", wrapped("{0, ", depth, "1", "}")),
        ("compare.py", wrapped("(0 == ", depth, "0", ")")),
        ("starred.py", wrapped("f(*[", depth, "0", "])")),
        ("named.py", wrapped("(x := ", depth, "0", ")")),
        ("keyword.py", wrapped("f(a=", depth, "0", ")")),
        (
            "yield.py",
            format!(
                "def f():\n    return {}\n",
                wrapped("(yield ", depth, "0", ")").trim_end()
            ),
        ),
        (
            "yield_from.py",
            format!(
                "def f():\n    return {}\n",
                wrapped("(yield from ", depth, "g()", ")").trim_end()
            ),
        ),
        (
            "await.py",
            format!(
                "async def f():\n    return {}\n",
                wrapped("(await ", depth, "g()", ")").trim_end()
            ),
        ),
        ("attribute.py", format!("{}\n", attribute_chain(depth))),
        (
            "class_base.py",
            format!(
                "class C({}):\n    pass\n",
                wrapped("[", depth, "0", "]").trim_end()
            ),
        ),
        ("list.pyi", wrapped("[", depth, "0", "]")),
    ]
}

fn statement_cases(depth: usize) -> Vec<(&'static str, String)> {
    vec![
        ("function.py", indented("def f():", depth)),
        ("async_function.py", indented("async def f():", depth)),
        ("class.py", indented("class C:", depth)),
        ("if.py", indented("if x:", depth)),
        ("for.py", indented("for x in y:", depth)),
        ("async_for.py", indented("async for x in y:", depth)),
        ("while.py", indented("while x:", depth)),
        ("with.py", indented("with x:", depth)),
        ("async_with.py", indented("async with x:", depth)),
        ("try_finally.py", nested_handler("finally:", depth)),
        ("try_except.py", nested_handler("except E:", depth)),
        ("try_star.py", nested_handler("except* E:", depth)),
    ]
}

fn pattern_cases(depth: usize) -> Vec<(&'static str, String)> {
    vec![
        (
            "pattern_sequence.py",
            format!(
                "match x:\n case {}:\n  pass\n",
                wrapped("[", depth, "0", "]").trim_end()
            ),
        ),
        (
            "pattern_class.py",
            format!(
                "match x:\n case {}:\n  pass\n",
                wrapped("C(", depth, "", ")").trim_end()
            ),
        ),
        (
            "pattern_or.py",
            format!(
                "match x:\n case {}:\n  pass\n",
                wrapped("(0|", depth, "0", ")").trim_end()
            ),
        ),
    ]
}

fn decorator_call(depth: usize) -> String {
    format!("@{}()\ndef f():\n    pass\n", attribute_chain(depth))
}

fn default_call(depth: usize) -> String {
    format!("def f(x={}()):\n    pass\n", attribute_chain(depth))
}

#[test]
fn deep_expressions_return_tool_error() {
    assert_tool_errors(None, &expression_cases(DEEP_EXPR));
}

#[test]
fn deep_statements_return_tool_error() {
    assert_tool_errors(None, &statement_cases(DEEP_STMT));
}

#[test]
fn deep_patterns_return_tool_error() {
    assert_tool_errors(None, &pattern_cases(DEEP_PATTERN));
}

#[test]
fn docstring_only_walk_returns_tool_error() {
    let mut cases = vec![
        ("list.py", wrapped("[", DEEP_EXPR, "0", "]")),
        ("function.py", indented("def f():", DEEP_STMT)),
        ("attribute.py", format!("{}\n", attribute_chain(DEEP_EXPR))),
    ];
    cases.extend(pattern_cases(DEEP_PATTERN));
    assert_tool_errors(Some("\"docstring\""), &cases);
}

#[test]
fn schema_only_walk_returns_tool_error() {
    let mut cases = vec![
        ("list.py", wrapped("[", DEEP_EXPR, "0", "]")),
        ("if.py", indented("if x:", DEEP_STMT)),
        ("decorator.py", decorator_call(DEEP_EXPR)),
        ("default.py", default_call(DEEP_EXPR)),
    ];
    cases.extend(pattern_cases(DEEP_PATTERN));
    assert_tool_errors(Some("\"schema-description\""), &cases);
}

#[test]
fn deep_indentation_with_open_string_returns_tool_error() {
    assert_tool_errors(
        None,
        &[("indented_string.py", indented_with_open_string(DEEP_INDENT))],
    );
}

#[test]
fn ordinary_nesting_still_completes() {
    let root = tempdir().unwrap();
    let config = public_interface(None);
    let mut sources = expression_cases(SAFE_DEPTH);
    sources.extend(statement_cases(SAFE_DEPTH));
    sources.extend(pattern_cases(SAFE_DEPTH));
    sources.push((
        "documented.py",
        format!(
            "\"\"\"module\"\"\"\n{}\n",
            wrapped("[", SAFE_DEPTH, "0", "]").trim_end()
        ),
    ));
    sources.push((
        "documented_if.py",
        format!("\"\"\"module\"\"\"\n{}", indented("if x:", SAFE_DEPTH)),
    ));
    sources.push(("indented_string.py", indented_with_open_string(SAFE_DEPTH)));
    for (name, source) in &sources {
        let documented = name.starts_with("documented");
        let output = scan(root.path(), &config, name, source);
        assert_eq!(
            output.status.code(),
            Some(i32::from(documented)),
            "{name}: status {:?} stderr {} stdout {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        let report: Report = serde_json::from_slice(&output.stdout).expect("JSON report");
        assert!(report.error.is_none(), "{name}: {report:?}");
        if documented {
            assert_eq!(report.findings.len(), 1, "{name}");
        } else {
            assert!(report.findings.is_empty(), "{name}: {report:?}");
        }
    }
}

#[test]
fn skipped_python_files_are_not_rejected_for_nesting() {
    let list = wrapped("[", DEEP_EXPR, "0", "]");
    let root = tempdir().unwrap();
    fs::create_dir(root.path().join("sdk")).unwrap();
    let allowed = scan(root.path(), &public_interface(None), "sdk/nested.py", &list);
    assert_eq!(allowed.status.code(), Some(0), "{allowed:?}");
    let disabled = scan(root.path(), "", "nested.py", &list);
    assert_eq!(disabled.status.code(), Some(0), "{disabled:?}");
}
