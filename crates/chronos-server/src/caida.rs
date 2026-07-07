//! CAIDA AS-relationship dataset acquisition.
//!
//! In the happy path the operator configures nothing: on startup Chronos
//! discovers the latest CAIDA serial-1 `as-rel` dataset, downloads it once into
//! the writable data directory, decompresses it, and reuses the cached copy on
//! subsequent runs (the filenames are date-stamped, so a new month yields a new
//! cache entry automatically). Operators can override this by pointing
//! `CHRONOS_CAIDA_ASREL` at a mounted file, by pinning an exact
//! `CHRONOS_CAIDA_URL`, or by disabling auto-download entirely.
//!
//! Acquisition is best-effort: any failure (no network in CI, an unreachable
//! mirror, a malformed listing) is logged and folded into `None`, and the caller
//! falls back to the degree heuristic. It never blocks startup indefinitely nor
//! panics.
//!
//! Style note: comments avoid em dashes; they use colons, semicolons, and
//! parentheses instead.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::config::AppConfig;

/// Upper bound on the whole acquisition (discovery plus download plus
/// decompression). Startup falls back to the heuristic if this elapses.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(90);

/// Per-request connect timeout for the HTTP client.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Resolve a decompressed CAIDA `as-rel` text file, downloading and caching it
/// when necessary.
///
/// Returns the path to a plain-text dataset ready for `parse_caida_as_rel`, or
/// `None` when no dataset could be obtained (auto-download disabled, or every
/// source failed). This function never returns an error: failures degrade to
/// `None` so startup always proceeds.
pub async fn resolve_dataset(config: &AppConfig) -> Option<PathBuf> {
    match tokio::time::timeout(ACQUIRE_TIMEOUT, resolve_inner(config)).await {
        Ok(Ok(Some(path))) => Some(path),
        Ok(Ok(None)) => None,
        Ok(Err(err)) => {
            warn!(
                error = %format!("{err:#}"),
                "chronos: CAIDA dataset acquisition failed; using degree heuristic"
            );
            None
        }
        Err(_) => {
            warn!(
                timeout_secs = ACQUIRE_TIMEOUT.as_secs(),
                "chronos: CAIDA dataset acquisition timed out; using degree heuristic"
            );
            None
        }
    }
}

async fn resolve_inner(config: &AppConfig) -> Result<Option<PathBuf>> {
    // 1. An explicitly mounted file wins. It may be plain text or bz2.
    if let Some(path) = &config.caida_as_rel {
        info!(path = %path.display(), "chronos: using mounted CAIDA dataset");
        return prepare_local(path, &config.data_dir).map(Some);
    }

    // 2. A pinned URL (for reproducible or point-in-time datasets).
    if let Some(url) = &config.caida_url {
        return download_and_cache(url, &config.data_dir).await.map(Some);
    }

    // 3. Auto-discover the latest dataset (the zero-config happy path).
    if !config.caida_autodownload {
        info!(
            "chronos: CAIDA auto-download disabled and no dataset configured; \
             using degree based relationship heuristic"
        );
        return Ok(None);
    }
    let url = discover_latest(&config.caida_base_url).await?;
    download_and_cache(&url, &config.data_dir).await.map(Some)
}

/// Prepare a mounted dataset for use. Plain-text files are used in place; bz2
/// files are decompressed once into the cache.
fn prepare_local(path: &Path, data_dir: &Path) -> Result<PathBuf> {
    if !path.exists() {
        anyhow::bail!(
            "configured CHRONOS_CAIDA_ASREL '{}' does not exist",
            path.display()
        );
    }
    if path.extension().and_then(|e| e.to_str()) != Some("bz2") {
        return Ok(path.to_path_buf());
    }

    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .context("mounted CAIDA path has no filename")?;
    let cache_path = cache_path_for(data_dir, file_name);
    if is_nonempty_file(&cache_path) {
        return Ok(cache_path);
    }
    let compressed = std::fs::read(path)
        .with_context(|| format!("reading mounted CAIDA dataset '{}'", path.display()))?;
    let plain = decompress_bz2(&compressed)?;
    write_atomically(&cache_path, &plain)?;
    info!(path = %cache_path.display(), "chronos: decompressed mounted CAIDA dataset into cache");
    Ok(cache_path)
}

/// Download a dataset (decompressing when the URL ends in `.bz2`) and store the
/// plain text under `<data_dir>/cache/caida`, reusing an existing cache entry.
async fn download_and_cache(url: &str, data_dir: &Path) -> Result<PathBuf> {
    let file_name = url
        .rsplit('/')
        .find(|s| !s.is_empty())
        .context("could not derive a filename from the CAIDA URL")?;
    let cache_path = cache_path_for(data_dir, file_name);
    if is_nonempty_file(&cache_path) {
        info!(path = %cache_path.display(), "chronos: using cached CAIDA dataset");
        return Ok(cache_path);
    }

    info!(%url, "chronos: downloading CAIDA dataset");
    let client = http_client()?;
    let bytes = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("requesting CAIDA dataset '{url}'"))?
        .error_for_status()
        .with_context(|| format!("CAIDA dataset '{url}' returned an error status"))?
        .bytes()
        .await
        .with_context(|| format!("reading CAIDA dataset body from '{url}'"))?;

    let compressed = file_name.ends_with(".bz2");
    let target = cache_path.clone();
    // Decompression and disk writes are CPU/IO bound; keep them off the async
    // reactor. This runs once at startup, not in any hot loop.
    tokio::task::spawn_blocking(move || -> Result<()> {
        let plain = if compressed {
            decompress_bz2(&bytes)?
        } else {
            bytes.to_vec()
        };
        write_atomically(&target, &plain)
    })
    .await
    .context("CAIDA cache writer task panicked")??;

    info!(path = %cache_path.display(), "chronos: cached CAIDA dataset");
    Ok(cache_path)
}

/// Discover the latest `as-rel` dataset filename from a CAIDA directory index.
async fn discover_latest(base_url: &str) -> Result<String> {
    let client = http_client()?;
    let html = client
        .get(base_url)
        .send()
        .await
        .with_context(|| format!("listing CAIDA directory '{base_url}'"))?
        .error_for_status()
        .with_context(|| format!("CAIDA directory '{base_url}' returned an error status"))?
        .text()
        .await
        .with_context(|| format!("reading CAIDA directory listing '{base_url}'"))?;

    let name = latest_as_rel_filename(&html)
        .context("no CAIDA as-rel dataset found in the directory listing")?;
    let separator = if base_url.ends_with('/') { "" } else { "/" };
    info!(file = %name, "chronos: discovered latest CAIDA dataset");
    Ok(format!("{base_url}{separator}{name}"))
}

/// Extract the newest `YYYYMMDD.as-rel[2].txt.bz2` filename from an HTML
/// directory listing.
///
/// This makes a deliberately conservative assumption about CAIDA's naming: the
/// filename begins with an 8-digit `YYYYMMDD` date, contains the `.as-rel.` or
/// `.as-rel2.` marker, and ends in `.bz2`. Derivative files (`v6-stable`,
/// `ppdc-ases`, `all-paths`) are excluded. The lexicographic maximum of the
/// zero-padded date prefix is the newest release.
fn latest_as_rel_filename(html: &str) -> Option<String> {
    let mut best: Option<(&str, &str)> = None;
    for piece in html.split("href=\"").skip(1) {
        let Some(end) = piece.find('"') else {
            continue;
        };
        let name = &piece[..end];
        if !name.ends_with(".txt.bz2") {
            continue;
        }
        if name.contains(".v6-") {
            continue;
        }
        if !(name.contains(".as-rel.") || name.contains(".as-rel2.")) {
            continue;
        }
        let Some((date, _)) = name.split_once('.') else {
            continue;
        };
        if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        match best {
            Some((best_date, _)) if best_date >= date => {}
            _ => best = Some((date, name)),
        }
    }
    best.map(|(_, name)| name.to_string())
}

/// Cache path for a source filename, dropping a trailing `.bz2` so the stored
/// copy carries a `.txt` extension.
fn cache_path_for(data_dir: &Path, file_name: &str) -> PathBuf {
    let plain_name = file_name.strip_suffix(".bz2").unwrap_or(file_name);
    data_dir.join("cache").join("caida").join(plain_name)
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!(
            "chronos/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/ToolchainOSS/Chronos)"
        ))
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .context("building the HTTP client for CAIDA acquisition")
}

fn decompress_bz2(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = bzip2::read::BzDecoder::new(bytes);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .context("decompressing the bzip2 CAIDA dataset")?;
    Ok(out)
}

/// Write bytes to `path` via a temporary file and a rename, so a crash mid-write
/// never leaves a truncated cache entry that a later run would trust.
fn write_atomically(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .context("cache path has no parent directory")?;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating CAIDA cache directory '{}'", dir.display()))?;
    let tmp = path.with_extension("download.tmp");
    std::fs::write(&tmp, data)
        .with_context(|| format!("writing CAIDA cache temp file '{}'", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("finalizing CAIDA cache file '{}'", path.display()))?;
    Ok(())
}

fn is_nonempty_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"
        <a href="20260301.as-rel.txt.bz2">20260301.as-rel.txt.bz2</a>
        <a href="20260301.as-rel.v6-stable.txt.bz2">v6</a>
        <a href="20260701.as-rel.txt.bz2">20260701.as-rel.txt.bz2</a>
        <a href="20260701.ppdc-ases.txt.bz2">ppdc</a>
        <a href="20260701.all-paths.bz2">paths</a>
        <a href="20260601.as-rel.txt.bz2">20260601.as-rel.txt.bz2</a>
        <a href="README.txt">README.txt</a>
    "#;

    #[test]
    fn picks_newest_as_rel_and_ignores_derivatives() {
        assert_eq!(
            latest_as_rel_filename(LISTING).as_deref(),
            Some("20260701.as-rel.txt.bz2")
        );
    }

    #[test]
    fn accepts_serial2_naming() {
        let html = r#"<a href="20260701.as-rel2.txt.bz2">x</a>"#;
        assert_eq!(
            latest_as_rel_filename(html).as_deref(),
            Some("20260701.as-rel2.txt.bz2")
        );
    }

    #[test]
    fn returns_none_when_no_dataset_present() {
        assert_eq!(latest_as_rel_filename("<a href=\"README.txt\">r</a>"), None);
    }

    #[test]
    fn cache_path_drops_bz2_and_nests_under_data_dir() {
        let p = cache_path_for(Path::new("/data"), "20260701.as-rel.txt.bz2");
        assert_eq!(p, Path::new("/data/cache/caida/20260701.as-rel.txt"));
    }

    #[test]
    fn bz2_roundtrip_decompresses() {
        // "hello\n" compressed with bzip2, then decompressed back.
        let plain = b"1|2|-1\n3|4|0\n";
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        use std::io::Write;
        encoder.write_all(plain).unwrap();
        let compressed = encoder.finish().unwrap();
        assert_eq!(decompress_bz2(&compressed).unwrap(), plain);
    }
}
