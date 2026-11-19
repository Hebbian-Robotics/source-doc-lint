use std::{
    collections::HashMap,
    fs,
    path::Path,
    process::{Command, Output},
};

use serde::Deserialize;
use tempfile::tempdir;

#[derive(Deserialize)]
struct Case {
    name: String,
    extension: String,
    source: String,
    findings: usize,
    #[serde(default)]
    directory: String,
}

#[derive(Deserialize)]
struct Report {
    reason: Option<String>,
    findings: Vec<Finding>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct Finding {
    file: String,
    #[serde(rename = "ruleId")]
    rule_id: String,
}

fn check(root: &Path, paths: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_source-doc-lint"))
        .current_dir(root)
        .arg("--json")
        .args(paths)
        .output()
        .expect("run binary")
}

fn read_report(output: &Output, status: i32) -> Report {
    assert_eq!(
        output.status.code(),
        Some(status),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON report")
}

fn all_cases() -> impl Iterator<Item = (bool, Case)> {
    [
        (true, include_str!("fixtures/cases.json")),
        (false, include_str!("fixtures/documentation_cases.json")),
        (false, include_str!("fixtures/schema_metadata_cases.json")),
    ]
    .into_iter()
    .flat_map(|(classification, fixture)| {
        serde_json::from_str::<Vec<Case>>(fixture)
            .expect("fixture")
            .into_iter()
            .map(move |case| (classification, case))
    })
}

#[test]
fn detects_expected_findings_for_each_existing_fixture() {
    let project = tempdir().unwrap();
    let root = project.path();
    fs::write(
        root.join("source-doc-lint.toml"),
        include_str!("fixtures/policy.toml"),
    )
    .unwrap();
    let expected: HashMap<_, _> = all_cases()
        .map(|(classification, case)| {
            let relative =
                Path::new(&case.directory).join(format!("{}.{}", case.name, case.extension));
            let path = root.join(&relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, case.source).unwrap();
            (relative, (classification, case.findings))
        })
        .collect();
    let report = read_report(&check(root, &[]), 1);
    let mut actual = HashMap::new();
    for finding in report.findings {
        let path = Path::new(&finding.file);
        let relative = path.strip_prefix(".").unwrap_or(path);
        let classification = finding.rule_id.starts_with("explanatory-comment-");
        if expected
            .get(relative)
            .is_some_and(|(rule, _)| *rule != classification)
        {
            continue;
        }
        *actual
            .entry((relative.to_owned(), classification))
            .or_insert(0) += 1;
    }
    for (path, (classification, count)) in expected {
        assert_eq!(
            actual.remove(&(path.clone(), classification)).unwrap_or(0),
            count,
            "{}",
            path.display()
        );
    }
    assert!(actual.is_empty(), "{actual:?}");
}

#[test]
fn applies_independent_path_policies_and_limits_selected_scans() {
    let project = tempdir().unwrap();
    let root = project.path();
    fs::write(
        root.join("source-doc-lint.toml"),
        include_str!("fixtures/policy.toml"),
    )
    .unwrap();
    for directory in ["sdk", "nested/sdk", "sdk-extras", "examples", "generated"] {
        fs::create_dir_all(root.join(directory)).unwrap();
        fs::write(
            root.join(directory).join("sample.py"),
            "# An ordinary comment\n\"Docstring\"\n",
        )
        .unwrap();
    }
    for (directory, expected) in [
        ("sdk", 0),
        ("nested/sdk", 1),
        ("sdk-extras", 2),
        ("examples", 1),
        ("generated", 1),
    ] {
        let report = read_report(&check(root, &[directory]), i32::from(expected > 0));
        assert_eq!(report.findings.len(), expected, "{directory}");
        if directory != "sdk-extras" {
            assert!(
                report
                    .findings
                    .iter()
                    .all(|finding| finding.rule_id == "documentation-python")
            );
        }
    }
}

#[test]
fn policy_is_opt_in_and_configuration_errors_fail_with_json() {
    let project = tempdir().unwrap();
    let root = project.path();
    fs::write(root.join("sample.py"), "# Unclassified\n\"Docstring\"\n").unwrap();
    let report = read_report(&check(root, &[]), 0);
    assert!(report.reason.is_none() && report.findings.is_empty());

    fs::write(
        root.join("source-doc-lint.toml"),
        r#"
[classification]
labels = ["REASON"]
extra_directives = ["^# custom-tool:"]
"#,
    )
    .unwrap();
    fs::write(
        root.join("sample.py"),
        "# custom-tool: ignore\n# REASON: Kept.\n# continuation\n\"Docstring\"\n",
    )
    .unwrap();
    assert!(read_report(&check(root, &[]), 0).findings.is_empty());

    fs::write(
        root.join("source-doc-lint.toml"),
        "[classification]\nlabels = []\n",
    )
    .unwrap();
    let report = read_report(&check(root, &[]), 2);
    assert!(report.error.unwrap().contains("classification.labels"));
    assert!(report.findings.is_empty());
}
