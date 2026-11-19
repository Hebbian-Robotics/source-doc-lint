mod check;
mod policy;
mod syntax;

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use ignore::WalkBuilder;
use serde::Serialize;

use crate::{
    check::Finding,
    policy::{Configuration, Policy},
    syntax::Language,
};

#[derive(Parser)]
#[command(
    version,
    about = "Check source comments and documentation against project policy"
)]
struct Arguments {
    #[arg(
        long,
        help = "Read policy from this file and resolve its paths relative to the file's directory"
    )]
    config: Option<PathBuf>,
    #[arg(long, help = "Write a JSON report")]
    json: bool,
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,
}

#[derive(Serialize)]
struct Report<'a> {
    reason: Option<&'a str>,
    findings: Vec<Finding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn configuration(config: Option<&Path>) -> Result<(PathBuf, Policy)> {
    let default_path = Path::new("source-doc-lint.toml");
    let config_path = config.unwrap_or(default_path);
    let raw = match fs::read_to_string(config_path) {
        Ok(raw) => raw,
        Err(error) if config.is_none() && error.kind() == io::ErrorKind::NotFound => {
            return Ok((
                PathBuf::from(".").canonicalize()?,
                Configuration::default().try_into()?,
            ));
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", config_path.display()));
        }
    };
    let policy = Policy::try_from(toml::from_str::<Configuration>(&raw)?)
        .with_context(|| format!("loading {}", config_path.display()))?;
    Ok((
        config_path
            .canonicalize()?
            .parent()
            .context("configuration has no containing directory")?
            .to_owned(),
        policy,
    ))
}

fn scan(root: &Path, policy: &Policy, paths: &[PathBuf]) -> Result<Vec<Finding>> {
    let mut walk = WalkBuilder::new(&paths[0]);
    walk.hidden(false).sort_by_file_path(Path::cmp);
    for path in &paths[1..] {
        walk.add(path);
    }
    let mut visited = std::collections::HashSet::new();
    let mut findings = Vec::new();
    for entry in walk.build() {
        let entry = entry?;
        if !entry
            .file_type()
            .is_some_and(|file_type| file_type.is_file())
        {
            continue;
        }
        let path = entry.path();
        let Some(language) = Language::from_path(path) else {
            continue;
        };
        let absolute_path = path.canonicalize()?;
        let project_path = absolute_path.strip_prefix(root).unwrap_or(&absolute_path);
        if policy.exclude.is_match(project_path) || !visited.insert(absolute_path.clone()) {
            continue;
        }
        let source =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        findings.extend(
            check::check_source(policy, language, path, project_path, &source)
                .with_context(|| format!("checking {}", path.display()))?,
        );
    }
    findings.sort_by(|left, right| {
        (
            &left.file,
            left.range.byte_offset.start,
            left.rule_id.as_str(),
        )
            .cmp(&(
                &right.file,
                right.range.byte_offset.start,
                right.rule_id.as_str(),
            ))
    });
    Ok(findings)
}

fn write_report(json: bool, report: Report<'_>) -> Result<ExitCode> {
    let exit = if report.error.is_some() {
        2
    } else {
        u8::from(!report.findings.is_empty())
    };
    let mut stdout = io::stdout().lock();
    if json {
        serde_json::to_writer(&mut stdout, &report)?;
        writeln!(stdout)?;
    } else {
        for finding in &report.findings {
            writeln!(
                stdout,
                "{}:{}:{}: {}",
                finding.file.display(),
                finding.range.start.line + 1,
                finding.range.start.column + 1,
                finding.action
            )?;
        }
        if let Some(error) = report.error {
            eprintln!("{error}");
        } else {
            writeln!(
                stdout,
                "{} comment or documentation policy violations.",
                report.findings.len()
            )?;
        }
        if let Some(reason) = report.reason {
            eprintln!("{reason}");
        }
    }
    Ok(ExitCode::from(exit))
}

fn run(arguments: Arguments) -> Result<ExitCode> {
    let outcome = configuration(arguments.config.as_deref()).and_then(|(root, policy)| {
        ensure!(!arguments.paths.is_empty(), "provide at least one path");
        let scan = scan(&root, &policy, &arguments.paths);
        Ok((policy.reason, scan))
    });
    let (reason, scan) = match outcome {
        Ok((reason, scan)) => (Some(reason), scan),
        Err(error) => (None, Err(error)),
    };
    let report = match scan {
        Ok(findings) => Report {
            reason: if findings.is_empty() {
                None
            } else {
                reason.as_deref()
            },
            findings,
            error: None,
        },
        Err(error) => Report {
            reason: reason.as_deref(),
            findings: Vec::new(),
            error: Some(format!("{error:#}")),
        },
    };
    write_report(arguments.json, report)
}

fn main() -> ExitCode {
    match run(Arguments::parse()) {
        Ok(exit) => exit,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(2)
        }
    }
}
