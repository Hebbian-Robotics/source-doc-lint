#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "These diagnostics tests fail immediately when fixture setup or the report contract breaks."
)]

//! Text diagnostics must show policy reasons, file paths, and errors as data.
//!
//! Untrusted trees can store terminal control characters in `reason` and in
//! file names. The text reporter has to print an escaped visible form of those
//! characters. The JSON reporter keeps the original characters inside the
//! structured serializer. The violation count is a real trailing summary line,
//! so a crafted path or reason cannot reprint it.

use std::{fmt::Write as _, fs, path::Path, process::Command};

use serde_json::Value;
use tempfile::tempdir;

const COMMENT_ACTION: &str = "Remove this comment or label the reason it must remain.";

fn lint(root: &Path, json: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_source-doc-lint"));
    command.current_dir(root);
    if json {
        command.arg("--json");
    }
    command.output().expect("run source-doc-lint")
}

fn toml_basic_string(value: &str) -> String {
    let mut encoded = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            other if other.is_control() => {
                write!(encoded, "\\u{:04X}", u32::from(other)).expect("string formatting succeeds");
            }
            other => encoded.push(other),
        }
    }
    encoded.push('"');
    encoded
}

fn write_policy(root: &Path, reason: &str) {
    let contents = format!(
        "reason = {}\n[classification]\nlabels = [\"WHY\"]\n",
        toml_basic_string(reason)
    );
    fs::write(root.join("source-doc-lint.toml"), contents).unwrap();
}

fn escaped(text: &str) -> String {
    text.escape_default().to_string()
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn json_document(bytes: &[u8]) -> &[u8] {
    let mut document = bytes;
    if let Some(stripped) = document.strip_suffix(b"\n") {
        document = stripped;
    }
    if let Some(stripped) = document.strip_suffix(b"\r") {
        document = stripped;
    }
    document
}

fn line_body(line: &str) -> &str {
    #[cfg(windows)]
    {
        line.strip_suffix('\r').unwrap_or(line)
    }
    #[cfg(not(windows))]
    {
        line
    }
}

fn assert_only_structural_controls(bytes: &[u8], stream: &str) {
    let text = String::from_utf8(bytes.to_vec())
        .unwrap_or_else(|error| panic!("{stream} is not valid UTF-8 ({error}): {bytes:?}"));
    for (index, line) in text.split('\n').enumerate() {
        let body = line_body(line);
        assert!(
            !body.chars().any(char::is_control),
            "{stream} line {index} contains a terminal control character: {body:?}"
        );
    }
}

fn assert_text_contract(output: &std::process::Output, status: i32) {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert_only_structural_controls(&output.stdout, "stdout");
    assert_only_structural_controls(&output.stderr, "stderr");
}

#[test]
fn text_report_escapes_control_characters_in_policy_reason() {
    let project = tempdir().unwrap();
    let root = project.path();
    let reason = "policy\r\u{1b}[2J\u{1b}[Hcleared\u{7}\u{1b}]0;forged\u{7}";
    write_policy(root, reason);
    fs::write(root.join("sample.py"), "# ordinary comment\n").unwrap();

    let output = lint(root, false);
    assert_text_contract(&output, 1);
    let stdout = normalize_newlines(&String::from_utf8(output.stdout).unwrap());
    let stderr = normalize_newlines(&String::from_utf8(output.stderr).unwrap());
    let sample_path = Path::new(".").join("sample.py");
    let diagnostic = format!("{}:1:1: {COMMENT_ACTION}", sample_path.display());
    assert_eq!(
        stdout,
        format!("{diagnostic}\n1 comment or documentation policy violations.\n")
    );
    assert_eq!(stderr, format!("{}\n", escaped(reason)));
    assert!(
        !stderr.contains(reason),
        "stderr still contains the raw policy reason"
    );
}

#[cfg(unix)]
#[test]
fn text_report_escapes_control_characters_in_finding_paths() {
    let project = tempdir().unwrap();
    let root = project.path();
    let reason = "Follow the comment and documentation rules configured for this project.";
    write_policy(root, reason);
    let file_name = "note\u{1b}[2J\rlabel.py";
    fs::write(root.join(file_name), "# ordinary comment\n").unwrap();

    let output = lint(root, false);
    assert_text_contract(&output, 1);
    let stdout = normalize_newlines(&String::from_utf8(output.stdout).unwrap());
    let stderr = normalize_newlines(&String::from_utf8(output.stderr).unwrap());
    let diagnostic = format!(
        "{}:1:1: {COMMENT_ACTION}",
        escaped(&format!("./{file_name}"))
    );
    assert_eq!(
        stdout,
        format!("{diagnostic}\n1 comment or documentation policy violations.\n")
    );
    assert_eq!(stderr, format!("{reason}\n"));
    assert!(
        !stdout.contains(file_name),
        "stdout still contains the raw file path"
    );
}

#[cfg(unix)]
#[test]
fn text_report_escapes_control_characters_in_read_errors() {
    let project = tempdir().unwrap();
    let root = project.path();
    let reason = "policy\r\u{1b}[1Aforged summary";
    write_policy(root, reason);
    let file_name = "bad\u{1b}[1A.py";
    fs::write(root.join(file_name), b"\xff").unwrap();

    let output = lint(root, false);
    assert_text_contract(&output, 2);
    let stderr = normalize_newlines(&String::from_utf8(output.stderr).unwrap());
    assert!(
        output.stdout.is_empty(),
        "read errors do not print findings"
    );
    assert!(
        stderr.contains(&escaped(&format!("./{file_name}"))),
        "read error does not show the escaped path: {stderr:?}"
    );
    assert!(
        stderr.contains("valid UTF-8"),
        "read error dropped the UTF-8 failure: {stderr:?}"
    );
    assert!(
        stderr.contains(&escaped(reason)),
        "read error still prints the raw policy reason: {stderr:?}"
    );
    assert!(!stderr.contains(file_name) && !stderr.contains(reason));
}

#[test]
fn text_report_escapes_control_characters_quoted_from_configuration() {
    let project = tempdir().unwrap();
    let root = project.path();
    let mut raw = b"reason = \"hi".to_vec();
    raw.extend_from_slice(b"\x1b[2JINJECT\"\n[classification]\nlabels = [\"WHY\"]\n");
    fs::write(root.join("source-doc-lint.toml"), raw).unwrap();
    fs::write(root.join("sample.py"), "# ordinary comment\n").unwrap();

    let output = lint(root, false);
    assert_text_contract(&output, 2);
    let stderr = normalize_newlines(&String::from_utf8(output.stderr).unwrap());
    assert!(stderr.contains("TOML parse error"), "{stderr:?}");
    assert!(
        stderr.contains(&escaped("hi\u{1b}[2JINJECT")),
        "configuration error quotes raw terminal controls: {stderr:?}"
    );
    assert!(!stderr.contains('\u{1b}'));
}

#[test]
fn json_report_keeps_control_characters_inside_structured_strings() {
    let project = tempdir().unwrap();
    let root = project.path();
    let reason = "policy\r\u{1b}[2J\u{1b}[Hcleared\u{7}";
    write_policy(root, reason);
    let file_name = if cfg!(unix) {
        "note\u{1b}[2J\rlabel.py"
    } else {
        "sample.py"
    };
    fs::write(root.join(file_name), "# ordinary comment\n").unwrap();

    let output = lint(root, true);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let document = json_document(&output.stdout);
    assert!(
        !document.contains(&0x1b) && !document.contains(&b'\r'),
        "JSON bytes contain raw terminal controls: {document:?}"
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["reason"], reason);
    let expected_path = Path::new(".").join(file_name);
    assert_eq!(
        report["findings"][0]["file"],
        expected_path.to_str().unwrap()
    );
    assert_eq!(report["findings"][0]["action"], COMMENT_ACTION);
    assert!(report.get("error").is_none());
}

#[test]
fn json_configuration_errors_escape_control_characters_in_the_document() {
    let project = tempdir().unwrap();
    let root = project.path();
    let mut raw = b"reason = \"hi".to_vec();
    raw.extend_from_slice(b"\x1b[2JINJECT\"\n[classification]\nlabels = [\"WHY\"]\n");
    fs::write(root.join("source-doc-lint.toml"), raw).unwrap();
    fs::write(root.join("sample.py"), "# ordinary comment\n").unwrap();

    let output = lint(root, true);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let document = json_document(&output.stdout);
    assert!(
        !document.contains(&0x1b),
        "JSON error document contains a raw escape: {document:?}"
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    let error = report["error"].as_str().expect("error string");
    assert!(error.contains("TOML parse error"), "{error:?}");
    assert!(error.contains("INJECT"), "{error:?}");
    assert!(error.contains('\u{1b}'));
    assert_eq!(
        report["findings"].as_array().unwrap().as_slice(),
        &[] as &[Value]
    );
}
