//! Fetching the `llama-server` the local provider runs.
//!
//! Nothing ships the binary. It used to be the user's problem — readiness said to put
//! `llama-server` on `PATH`, which asks somebody to change their environment for one
//! application, and a downloaded three-gigabyte model sat unusable until they did. This
//! module fetches it the same way the model catalog fetches a GGUF, into the same data
//! folder, with the same resumable download and the same progress events.
//!
//! **The build is pinned and the asset name is not guessed.** It was read off the live
//! release listing, which is the rule AD-5 already imposes on the model catalog for the
//! same reason: a plausible-looking URL that does not exist fails at the worst moment,
//! in front of a user who cannot tell why. Moving to a newer build is a release step —
//! change `BUILD`, check the asset still exists, and run the ignored gate below.

use std::path::{Path, PathBuf};

use crate::download::{Cancel, DownloadError, ProgressListener, Result, fetch_url};

/// The llama.cpp build this application fetches.
///
/// Pinned rather than resolved from "latest": the asset name carries the build number,
/// so a moving target means a URL this code cannot spell. Verified against the release
/// listing on 2026-08-24.
pub const BUILD: &str = "b10612";

/// Which of the thirteen Windows builds is fetched.
///
/// The plain x64 CPU build, at 18 MB against 146 MB for the smallest CUDA one and a
/// 390 MB runtime beside it. It runs on every x64 machine, which the accelerated builds
/// do not: CUDA needs an NVIDIA card, ROCm an AMD one, SYCL an Intel one, and Vulkan a
/// driver that may or may not be installed. Somebody who wants their GPU used can point
/// at their own build; everybody else gets one that works.
const ASSET: &str = "bin-win-cpu-x64";

/// What the server is called once extracted.
const SERVER: &str = "llama-server.exe";

/// How large the archive is, so the settings page can say what it is about to fetch
/// before the server has told it. Measured, not estimated.
pub const APPROXIMATE_BYTES: u64 = 18_067_753;

pub fn asset_name() -> String {
    format!("llama-{BUILD}-{ASSET}.zip")
}

pub fn download_url() -> String {
    format!(
        "https://github.com/ggml-org/llama.cpp/releases/download/{BUILD}/{}",
        asset_name()
    )
}

/// Where this build is unpacked. The build number is in the path so a later one lands
/// beside it rather than half-overwriting it.
pub fn runtime_dir() -> anyhow::Result<PathBuf> {
    Ok(oh_core::paths::data_dir()?.join("runtime").join(BUILD))
}

/// The fetched server, if it is there.
pub fn installed() -> Option<PathBuf> {
    let server = runtime_dir().ok()?.join(SERVER);
    server.is_file().then_some(server)
}

/// Download the release and unpack it, returning the server's path.
///
/// Re-fetching when it is already there is a no-op, so this is safe to call whenever a
/// local model is chosen.
pub async fn fetch(listener: Option<ProgressListener>, cancel: &Cancel) -> Result<PathBuf> {
    if let Some(server) = installed() {
        return Ok(server);
    }

    let dir = runtime_dir().map_err(|error| DownloadError::Transport(error.to_string()))?;
    let archive = dir.join(asset_name());
    fetch_url(&download_url(), &archive, listener, cancel).await?;

    unpack(&archive, &dir)?;

    // The archive is 18 MB of the same bytes now sitting unpacked beside it.
    let _ = std::fs::remove_file(&archive);

    installed()
        .ok_or_else(|| DownloadError::Transport(format!("{} was not in {}", SERVER, asset_name())))
}

/// Unpack every entry into `dir`, flat.
///
/// Everything is extracted rather than just `llama-server.exe`, because that file is a
/// nine-kilobyte launcher: the work is in `llama-server-impl.dll`, `llama.dll`,
/// `ggml-base.dll` and one `ggml-cpu-*.dll` per instruction set, chosen at run time
/// from the fifteen shipped. Picking a subset means guessing that list correctly on
/// every machine and every future build, and guessing wrong fails as a missing DLL at
/// spawn time.
fn unpack(archive: &Path, dir: &Path) -> Result<()> {
    let file = std::fs::File::open(archive).map_err(|source| DownloadError::Write {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|error| {
        DownloadError::Transport(format!("{} is not readable: {error}", asset_name()))
    })?;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|error| DownloadError::Transport(error.to_string()))?;

        // `enclosed_name` refuses anything that would land outside the directory, which
        // is what stops an archive from writing `..\..\somewhere else`. An entry it
        // will not vouch for is skipped rather than trusted.
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        let target = dir.join(&relative);

        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|source| DownloadError::Write {
                path: target.clone(),
                source,
            })?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|source| DownloadError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let mut out = std::fs::File::create(&target).map_err(|source| DownloadError::Write {
            path: target.clone(),
            source,
        })?;
        std::io::copy(&mut entry, &mut out).map_err(|source| DownloadError::Write {
            path: target.clone(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_names_the_pinned_build_in_both_places() {
        let url = download_url();
        assert!(url.contains(&format!("/download/{BUILD}/")), "{url}");
        assert!(
            url.ends_with(&format!("llama-{BUILD}-bin-win-cpu-x64.zip")),
            "{url}"
        );
    }

    /// The one thing no unit test can check: that the release is still published under
    /// this name. Run it before changing `BUILD`.
    ///
    /// ```text
    /// cargo test -p oh-inference --lib runtime -- --ignored
    /// ```
    #[tokio::test]
    #[ignore = "reaches the real GitHub release listing"]
    async fn the_pinned_asset_is_still_published() {
        let response = reqwest::Client::new()
            .head(download_url())
            .send()
            .await
            .expect("the release listing must answer");
        assert!(
            response.status().is_success(),
            "{} answered {}",
            download_url(),
            response.status()
        );
    }
}
