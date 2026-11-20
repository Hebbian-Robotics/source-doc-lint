use std::{
    fmt::Write as _,
    io::{Cursor, Read},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, ensure};
use flate2::{Compression, write::GzEncoder};
use self_update::http_client::{HeaderMap, HttpClient, HttpResponse};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{NAME, Update, update_from_release};

const TEST_CASE: &str = "SOURCE_DOC_LINT_UPDATE_TEST_CASE";
const TEST_NAME: &str =
    "update::tests::only_a_verified_platform_archive_replaces_the_running_executable";
const NEW_EXECUTABLE: &[u8] = b"verified executable payload from the release fixture";

#[test]
fn only_a_verified_platform_archive_replaces_the_running_executable() -> Result<()> {
    if let Ok(test_case) = std::env::var(TEST_CASE) {
        return run_fixture_update(&test_case);
    }

    let temporary = tempfile::tempdir()?;
    let candidate = temporary
        .path()
        .join(format!("{NAME}{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(std::env::current_exe()?, &candidate)?;
    let original = std::fs::read(&candidate)?;
    for (test_case, expected_failure) in [
        ("check", None),
        ("missing digest", Some("valid SHA-256 digest")),
        ("bad digest", Some("checksum mismatch")),
        ("install", None),
    ] {
        let result = Command::new(&candidate)
            .args(["--exact", TEST_NAME])
            .env(TEST_CASE, test_case)
            .output()?;
        let output = String::from_utf8_lossy(&result.stdout);
        assert_eq!(
            result.status.success(),
            expected_failure.is_none(),
            "{test_case}: {output}"
        );
        if let Some(message) = expected_failure {
            assert!(output.contains(message), "{test_case}: {output}");
        }
        if test_case == "install" {
            assert_eq!(std::fs::read(&candidate)?, NEW_EXECUTABLE);
        } else {
            ensure!(
                std::fs::read(&candidate)? == original,
                "{test_case} changed the executable"
            );
        }
    }
    Ok(())
}

fn run_fixture_update(test_case: &str) -> Result<()> {
    let archive_name = format!("{NAME}-{}.tar.gz", self_update::get_target());
    let archive = compressed_binary()?;
    let mut digest = String::from("sha256:");
    for byte in Sha256::digest(&archive) {
        write!(digest, "{byte:02x}")?;
    }
    let digest = match test_case {
        "missing digest" => None,
        "bad digest" => Some(format!("sha256:{}", "0".repeat(64))),
        _ => Some(digest),
    };
    let fixture = Arc::new(ReleaseFixture {
        metadata: serde_json::to_vec(&json!({
            "name": "Source Doc Lint test release", "tag_name": "v1.1.0",
            "created_at": "2026-01-01T00:00:00Z",
            "assets": [
                { "name": format!("other-{archive_name}"), "url": "https://updates.example/wrong" },
                { "name": archive_name, "url": "https://updates.example/archive", "digest": digest },
            ],
        }))?,
        archive,
        downloads: AtomicUsize::new(0),
    });
    let mut updater = Update::configure();
    updater
        .repo_owner("Hebbian-Robotics")
        .repo_name(NAME)
        .bin_name(NAME)
        .current_version("1.0.0")
        .http_client(Arc::clone(&fixture) as Arc<dyn HttpClient>);

    let result = update_from_release(&mut updater, "1.0.0", &archive_name, test_case == "check");
    let downloads = fixture.downloads.load(Ordering::Relaxed);
    match test_case {
        "check" => assert_eq!(downloads, 0, "checking downloaded the archive"),
        "missing digest" => assert_eq!(downloads, 0, "missing digest allowed an archive download"),
        _ => assert_eq!(downloads, 1, "expected the exact platform archive"),
    }
    result
}

fn compressed_binary() -> Result<Vec<u8>> {
    let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(NEW_EXECUTABLE.len())?);
    header.set_mode(0o755);
    archive.append_data(
        &mut header,
        format!("{NAME}{}", std::env::consts::EXE_SUFFIX),
        Cursor::new(NEW_EXECUTABLE),
    )?;
    Ok(archive.into_inner()?.finish()?)
}

struct ReleaseFixture {
    metadata: Vec<u8>,
    archive: Vec<u8>,
    downloads: AtomicUsize,
}

impl HttpClient for ReleaseFixture {
    fn get(
        &self,
        url: &str,
        _headers: &HeaderMap,
        _timeout: Option<Duration>,
    ) -> self_update::Result<Box<dyn HttpResponse>> {
        let data = if url.ends_with("/releases/latest") || url.ends_with("/releases/tags/v1.1.0") {
            &self.metadata
        } else if url == "https://updates.example/archive" {
            self.downloads.fetch_add(1, Ordering::Relaxed);
            &self.archive
        } else {
            return Err(self_update::Error::http_status_error(
                self_update::http::StatusCode::NOT_FOUND.as_u16(),
                url,
            ));
        };
        Ok(Box::new(FixtureResponse {
            headers: HeaderMap::new(),
            data: data.clone(),
        }))
    }
}

struct FixtureResponse {
    headers: HeaderMap,
    data: Vec<u8>,
}

impl HttpResponse for FixtureResponse {
    fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    fn body(self: Box<Self>) -> Box<dyn Read> {
        Box::new(Cursor::new(self.data))
    }
}
