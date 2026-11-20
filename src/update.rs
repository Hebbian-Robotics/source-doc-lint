use std::time::Duration;

use anyhow::{Context, Result};
use self_update::backends::github::{Update, UpdateBuilder};
use self_update::{ReleaseAsset, version::bump_is_greater};

use crate::terminal_text::EscapedTerminalText;

const NAME: &str = env!("CARGO_PKG_NAME");
const INSTALLED_VERSION: &str = env!("CARGO_PKG_VERSION");

struct AvailableUpdate {
    version: String,
    archive: ReleaseAsset,
}

enum UpdateAvailability {
    Current { latest_version: String },
    Available(AvailableUpdate),
}

pub fn run(check: bool) -> Result<()> {
    let mut updater = Update::configure();
    updater
        .repo_owner("Hebbian-Robotics")
        .repo_name(NAME)
        .bin_name(NAME)
        .current_version(INSTALLED_VERSION)
        .timeout(Duration::from_secs(90));

    let archive_name = format!("{NAME}-{}.tar.gz", self_update::get_target());
    update_from_release(&mut updater, INSTALLED_VERSION, &archive_name, check)
}

fn update_from_release(
    updater: &mut UpdateBuilder,
    installed_version: &str,
    archive_name: &str,
    check: bool,
) -> Result<()> {
    match check_for_update(updater, installed_version, archive_name)? {
        UpdateAvailability::Current { latest_version } => {
            println!(
                "{NAME} {installed_version} is up to date (latest published: {}).",
                EscapedTerminalText(&latest_version)
            );
        }
        UpdateAvailability::Available(available) if check => {
            println!(
                "{NAME} {} is available (installed: {installed_version}). Run {NAME} update to install it.",
                EscapedTerminalText(&available.version)
            );
        }
        UpdateAvailability::Available(available) => {
            println!(
                "Updating {NAME} from {installed_version} to {}.",
                EscapedTerminalText(&available.version)
            );
            let installed = install_update(updater, available)?;
            println!("Updated {NAME} to {}.", EscapedTerminalText(&installed));
        }
    }
    Ok(())
}

fn check_for_update(
    updater: &UpdateBuilder,
    installed_version: &str,
    archive_name: &str,
) -> Result<UpdateAvailability> {
    let releases = updater.build()?.get_latest_release()?;
    let release = releases
        .latest()
        .context("No published Source Doc Lint release found")?;
    let version = release.version();
    if !bump_is_greater(installed_version, version)? {
        return Ok(UpdateAvailability::Current {
            latest_version: version.to_owned(),
        });
    }

    let archive = release
        .assets()
        .iter()
        .find(|asset| asset.name() == archive_name)
        .with_context(|| format!("Source Doc Lint v{version} has no {archive_name} archive"))?;
    let published_sha256 = archive
        .digest()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .is_some_and(|hash| {
            hash.len() == 64 && hash.bytes().all(|digit| digit.is_ascii_hexdigit())
        });
    anyhow::ensure!(
        published_sha256,
        "GitHub did not publish a valid SHA-256 digest for {archive_name}"
    );
    Ok(UpdateAvailability::Available(AvailableUpdate {
        version: version.to_owned(),
        archive: archive.clone(),
    }))
}

fn install_update(updater: &mut UpdateBuilder, available: AvailableUpdate) -> Result<String> {
    let AvailableUpdate { version, archive } = available;
    let status = updater
        .release_tag(format!("v{version}"))
        .asset_matcher(move |assets| {
            // Check the digest again when fetching the chosen tag, before
            // allowing the download that may replace the executable.
            assets
                .iter()
                .find(|asset| asset.name() == archive.name() && asset.digest() == archive.digest())
                .cloned()
        })
        .verify_release_digest(true)
        .check_install_path_writable(true)
        .no_confirm(true)
        .build()?
        .update()
        .with_context(|| format!("Could not install Source Doc Lint v{version}"))?;
    Ok(status.version().to_owned())
}

#[cfg(test)]
mod tests;
