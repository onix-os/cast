// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{future::Future, io, os::unix::fs::OpenOptionsExt as _, path::Path, sync::OnceLock, time::Duration};

use futures_util::TryStreamExt;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use url::Url;

use crate::environment;

/// Shared client for TCP socket reuse and connection limits.
static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Sane ceiling for legacy/general artifact downloads.
///
/// Security-sensitive callers should use one of the `*_with_limits` functions
/// and select a smaller, domain-specific policy. The limit is enforced against
/// the decoded byte stream, even when a server omits or lies about
/// `Content-Length`.
pub const DEFAULT_DOWNLOAD_LIMITS: DownloadLimits = DownloadLimits::new(8 * GIB, Duration::from_secs(30 * 60));

/// Default policy for small JSON metadata documents.
pub const DEFAULT_JSON_DOWNLOAD_LIMITS: DownloadLimits = DownloadLimits::new(16 * MIB, Duration::from_secs(60));

/// Hard resource limits for one complete download operation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DownloadLimits {
    /// Maximum number of decoded bytes admitted from the resource.
    pub max_bytes: u64,
    /// Maximum elapsed time for response setup, transfer, flush, and install.
    pub total_timeout: Duration,
}

impl DownloadLimits {
    pub const fn new(max_bytes: u64, total_timeout: Duration) -> Self {
        Self {
            max_bytes,
            total_timeout,
        }
    }
}

fn get_client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::ClientBuilder::new()
            .referer(false)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                match validate_redirect(attempt.previous(), attempt.url()) {
                    Ok(()) => attempt.follow(),
                    Err(error) => attempt.error(error),
                }
            }))
            .user_agent(concat!("cast/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("build reqwest client")
    })
}

/// Preserve the security properties of the URL which the caller admitted.
///
/// Reqwest's default policy limits redirect count, but it permits a secure URL
/// to redirect to plaintext HTTP and permits authority-like URL components in
/// `Location`. Source hashes protect reproducibility after locking; they do
/// not make a transport downgrade or redirect-borne credential safe while a
/// lock is being created.
fn validate_redirect(previous: &[Url], next: &Url) -> Result<(), RedirectPolicyError> {
    // Match reqwest's default ten-hop ceiling. `previous` contains the initial
    // request as well as every URL already followed.
    if previous.len() > 10 {
        return Err(RedirectPolicyError::TooManyHops);
    }
    if !next.username().is_empty() || next.password().is_some() {
        return Err(RedirectPolicyError::EmbeddedCredentials);
    }
    if next.fragment().is_some() {
        return Err(RedirectPolicyError::Fragment);
    }
    if !matches!(next.scheme(), "http" | "https") || next.host_str().is_none() {
        return Err(RedirectPolicyError::UnsupportedTarget);
    }
    if previous.last().is_some_and(|url| url.scheme() == "https") && next.scheme() != "https" {
        return Err(RedirectPolicyError::TransportDowngrade);
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq, Eq)]
enum RedirectPolicyError {
    #[error("download redirect chain exceeds ten hops")]
    TooManyHops,
    #[error("download redirect target contains embedded credentials")]
    EmbeddedCredentials,
    #[error("download redirect target contains a fragment")]
    Fragment,
    #[error("download redirect target is not an absolute HTTP(S) URL")]
    UnsupportedTarget,
    #[error("download redirect attempts to downgrade HTTPS transport")]
    TransportDowngrade,
}

/// Downloads a file using [`DEFAULT_DOWNLOAD_LIMITS`].
pub async fn download(url: Url, to: &Path) -> Result<(), Error> {
    download_with_limits(url, to, DEFAULT_DOWNLOAD_LIMITS).await
}

/// Downloads a file under an explicit resource policy.
pub async fn download_with_limits(url: Url, to: &Path, limits: DownloadLimits) -> Result<(), Error> {
    download_from_source(fetch(url), to, limits, None, None)
        .await
        .map(|_| ())
}

/// Downloads and decodes JSON using [`DEFAULT_JSON_DOWNLOAD_LIMITS`].
pub async fn download_json<T: DeserializeOwned>(url: Url) -> Result<T, Error> {
    download_json_with_limits(url, DEFAULT_JSON_DOWNLOAD_LIMITS).await
}

/// Downloads and decodes JSON under an explicit resource policy.
pub async fn download_json_with_limits<T: DeserializeOwned>(url: Url, limits: DownloadLimits) -> Result<T, Error> {
    let bytes = read_source_with_limits(fetch(url), limits).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Downloads a file and returns its SHA-256 hash using
/// [`DEFAULT_DOWNLOAD_LIMITS`].
pub async fn download_with_sha256(url: Url, to: &Path) -> Result<String, Error> {
    download_with_sha256_and_limits(url, to, DEFAULT_DOWNLOAD_LIMITS).await
}

/// Downloads a file under an explicit resource policy and returns its SHA-256
/// hash.
pub async fn download_with_sha256_and_limits(url: Url, to: &Path, limits: DownloadLimits) -> Result<String, Error> {
    download_from_source(fetch(url), to, limits, None, None).await
}

/// Downloads a file using [`DEFAULT_DOWNLOAD_LIMITS`] and invokes
/// `on_progress` after each admitted chunk.
pub async fn download_with_progress(url: Url, to: &Path, on_progress: impl Fn(Progress) + Unpin) -> Result<(), Error> {
    download_with_progress_and_limits(url, to, DEFAULT_DOWNLOAD_LIMITS, on_progress).await
}

/// Downloads a file under an explicit resource policy and invokes
/// `on_progress` after each admitted chunk.
pub async fn download_with_progress_and_limits(
    url: Url,
    to: &Path,
    limits: DownloadLimits,
    on_progress: impl Fn(Progress) + Unpin,
) -> Result<(), Error> {
    download_from_source(fetch(url), to, limits, Some(&on_progress), None)
        .await
        .map(|_| ())
}

/// Downloads a file using [`DEFAULT_DOWNLOAD_LIMITS`], invokes `on_progress`
/// after each admitted chunk, and returns its SHA-256 hash.
pub async fn download_with_progress_and_sha256(
    url: Url,
    to: &Path,
    on_progress: impl Fn(Progress) + Unpin,
) -> Result<String, Error> {
    download_with_progress_and_sha256_and_limits(url, to, DEFAULT_DOWNLOAD_LIMITS, on_progress).await
}

/// Downloads a file under an explicit resource policy, invokes `on_progress`
/// after each admitted chunk, and returns its SHA-256 hash.
pub async fn download_with_progress_and_sha256_and_limits(
    url: Url,
    to: &Path,
    limits: DownloadLimits,
    on_progress: impl Fn(Progress) + Unpin,
) -> Result<String, Error> {
    download_from_source(fetch(url), to, limits, Some(&on_progress), None).await
}

/// Downloads a file under an explicit resource policy and publishes it only
/// when its SHA-256 hash matches `expected_sha256`.
///
/// The completed bytes remain in an unpredictable same-directory staging file
/// until the digest is accepted, so a mismatch never replaces the destination.
pub async fn download_with_progress_and_expected_sha256_and_limits(
    url: Url,
    to: &Path,
    expected_sha256: &str,
    limits: DownloadLimits,
    on_progress: impl Fn(Progress) + Unpin,
) -> Result<(), Error> {
    download_from_source(fetch(url), to, limits, Some(&on_progress), Some(expected_sha256))
        .await
        .map(|_| ())
}

struct Fetched {
    reader: Box<dyn AsyncRead + Unpin + Send>,
    content_length: Option<u64>,
}

/// Fetch a resource at the provided [`Url`] and return its reader and any
/// trustworthy-enough-to-preflight (but never sufficient) length hint.
async fn fetch(url: Url) -> Result<Fetched, Error> {
    validate_fetch_url(&url)?;
    if let Ok(path) = url.to_file_path() {
        // O_NONBLOCK prevents a file URL naming a FIFO/device from consuming a
        // blocking-pool thread beyond the operation timeout. O_NOFOLLOW closes
        // the final-component symlink race before descriptor inspection.
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(path)
            .await?;
        let metadata = file.metadata().await?;
        if !metadata.file_type().is_file() {
            return Err(Error::Read(io::Error::new(
                io::ErrorKind::InvalidInput,
                "local download resource is not a regular file",
            )));
        }
        let content_length = Some(metadata.len());
        Ok(Fetched {
            reader: Box::new(BufReader::with_capacity(environment::FILE_READ_BUFFER_SIZE, file)),
            content_length,
        })
    } else {
        http_get(url).await
    }
}

/// Admit only URL forms whose authority and local-path meaning are explicit.
///
/// Fragments are never sent to an HTTP server, and file-URL queries are not
/// part of the opened pathname. Accepting either would let two different
/// authored identities retrieve the same bytes. Credentials belong in an
/// explicit transport credential mechanism, not in a URL which can be copied
/// into diagnostics, locks, or process arguments.
fn validate_fetch_url(url: &Url) -> Result<(), Error> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::UrlPolicy {
            reason: "embedded credentials are not allowed",
        });
    }
    if url.fragment().is_some() {
        return Err(Error::UrlPolicy {
            reason: "fragments are not allowed",
        });
    }
    match url.scheme() {
        "http" | "https" if url.host_str().is_some() => Ok(()),
        "file" if url.query().is_none() && url.to_file_path().is_ok() => Ok(()),
        "file" => Err(Error::UrlPolicy {
            reason: "file URL must be an absolute local path without a query",
        }),
        _ => Err(Error::UrlPolicy {
            reason: "only absolute HTTP(S) and local file URLs are supported",
        }),
    }
}

/// Internal HTTP fetch helper.
async fn http_get(url: Url) -> Result<Fetched, Error> {
    let response = get_client().get(url).send().await?.error_for_status()?;
    let content_length = response.content_length();
    let stream = response.bytes_stream().map_err(io::Error::other);

    Ok(Fetched {
        // Convert the stream into an AsyncReader. This chunks the stream
        // automatically and gives compatibility with Tokio I/O functions.
        reader: Box::new(tokio_util::io::StreamReader::new(stream)),
        content_length,
    })
}

async fn download_from_source<F>(
    source: F,
    to: &Path,
    limits: DownloadLimits,
    on_progress: Option<&dyn Fn(Progress)>,
    expected_sha256: Option<&str>,
) -> Result<String, Error>
where
    F: Future<Output = Result<Fetched, Error>>,
{
    match tokio::time::timeout(limits.total_timeout, async {
        let fetched = source.await?;
        reject_announced_oversize(fetched.content_length, limits)?;
        write_fetched_to_file(fetched.reader, to, limits, on_progress, expected_sha256).await
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(Error::Timeout {
            timeout: limits.total_timeout,
        }),
    }
}

async fn write_fetched_to_file(
    mut reader: Box<dyn AsyncRead + Unpin + Send>,
    to: &Path,
    limits: DownloadLimits,
    on_progress: Option<&dyn Fn(Progress)>,
    expected_sha256: Option<&str>,
) -> Result<String, Error> {
    let parent = to
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = tempfile::Builder::new()
        .prefix(".cast-download-")
        .make_in(parent, |path| {
            // Preserve File::create's historical 0666-subject-to-umask mode
            // without returning to a predictable path. create_new maps to
            // O_EXCL, and Builder retries a fresh random name on collision.
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o666)
                .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
                .open(path)
        })
        .map_err(|source| Error::CreateStaging {
            parent: parent.to_owned(),
            source,
        })?;
    let (file, temporary_path) = temporary.into_parts();
    let mut out = tokio::fs::File::from_std(file);
    let mut hasher = Sha256::new();
    let mut completed = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];

    loop {
        // Read one byte beyond the remaining allowance. Content-Length is only
        // a hint; this is the authoritative N/N+1 enforcement point.
        let remaining = limits.max_bytes.saturating_sub(completed);
        let read_capacity = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = reader.read(&mut buffer[..read_capacity]).await?;
        if read == 0 {
            break;
        }
        if read as u64 > remaining {
            return Err(Error::TooLarge {
                limit: limits.max_bytes,
            });
        }

        out.write_all(&buffer[..read]).await?;
        hasher.update(&buffer[..read]);
        completed += read as u64;
        if let Some(callback) = on_progress {
            callback(Progress {
                delta: read as u64,
                completed,
            });
        }
    }

    out.flush().await?;
    out.sync_all().await?;
    let actual_sha256 = hex::encode(hasher.finalize());
    if let Some(expected_sha256) = expected_sha256
        && actual_sha256 != expected_sha256
    {
        return Err(Error::HashMismatch {
            expected: expected_sha256.to_owned(),
            actual: actual_sha256,
        });
    }

    // Close the async handle before the same-filesystem atomic replacement.
    drop(out);
    temporary_path.persist(to).map_err(|error| Error::Install {
        target: to.to_owned(),
        source: error.error,
    })?;
    let directory = tokio::fs::File::open(parent)
        .await
        .map_err(|source| Error::SyncInstallDirectory {
            directory: parent.to_owned(),
            source,
        })?;
    directory
        .sync_all()
        .await
        .map_err(|source| Error::SyncInstallDirectory {
            directory: parent.to_owned(),
            source,
        })?;

    Ok(actual_sha256)
}

async fn read_source_with_limits<F>(source: F, limits: DownloadLimits) -> Result<Vec<u8>, Error>
where
    F: Future<Output = Result<Fetched, Error>>,
{
    match tokio::time::timeout(limits.total_timeout, async {
        let mut fetched = source.await?;
        reject_announced_oversize(fetched.content_length, limits)?;
        let mut bytes = Vec::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut completed = 0_u64;

        loop {
            let remaining = limits.max_bytes.saturating_sub(completed);
            let read_capacity = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
            let read = fetched.reader.read(&mut buffer[..read_capacity]).await?;
            if read == 0 {
                break;
            }
            if read as u64 > remaining {
                return Err(Error::TooLarge {
                    limit: limits.max_bytes,
                });
            }
            bytes.extend_from_slice(&buffer[..read]);
            completed += read as u64;
        }

        Ok(bytes)
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(Error::Timeout {
            timeout: limits.total_timeout,
        }),
    }
}

fn reject_announced_oversize(content_length: Option<u64>, limits: DownloadLimits) -> Result<(), Error> {
    if content_length.is_some_and(|length| length > limits.max_bytes) {
        Err(Error::TooLarge {
            limit: limits.max_bytes,
        })
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("fetch")]
    Fetch(#[from] reqwest::Error),
    #[error("I/O")]
    Read(#[from] io::Error),
    #[error("download exceeds byte limit of {limit}")]
    TooLarge { limit: u64 },
    #[error("download exceeded total timeout of {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("download URL violates transport policy: {reason}")]
    UrlPolicy { reason: &'static str },
    #[error("download SHA-256 mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("create private download staging file in {parent:?}")]
    CreateStaging {
        parent: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("atomically install completed download at {target:?}")]
    Install {
        target: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync completed download directory at {directory:?}")]
    SyncInstallDirectory {
        directory: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decode JSON")]
    DecodeJson(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub delta: u64,
    pub completed: u64,
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, pin::Pin, task, time::Instant};

    use super::*;

    fn limits(max_bytes: u64) -> DownloadLimits {
        DownloadLimits::new(max_bytes, Duration::from_secs(5))
    }

    fn fetched(bytes: &[u8], content_length: Option<u64>) -> Fetched {
        Fetched {
            reader: Box::new(Cursor::new(bytes.to_vec())),
            content_length,
        }
    }

    #[test]
    fn redirects_preserve_secure_transport_and_reject_authority_components() {
        let secure = Url::parse("https://example.invalid/source").unwrap();
        assert!(validate_redirect(std::slice::from_ref(&secure), &secure).is_ok());

        let plaintext = Url::parse("http://example.invalid/source").unwrap();
        assert!(matches!(
            validate_redirect(std::slice::from_ref(&secure), &plaintext),
            Err(RedirectPolicyError::TransportDowngrade)
        ));
        assert!(validate_redirect(std::slice::from_ref(&plaintext), &plaintext).is_ok());

        for (target, expected) in [
            (
                "https://user:secret@example.invalid/source",
                RedirectPolicyError::EmbeddedCredentials,
            ),
            ("https://example.invalid/source#fragment", RedirectPolicyError::Fragment),
            ("file:///tmp/source", RedirectPolicyError::UnsupportedTarget),
        ] {
            let error = validate_redirect(std::slice::from_ref(&secure), &Url::parse(target).unwrap()).unwrap_err();
            assert_eq!(error, expected);
            assert!(!error.to_string().contains("secret"));
            assert!(!error.to_string().contains(target));
        }
    }

    #[test]
    fn initial_download_urls_reject_ambiguous_or_secret_components() {
        for accepted in [
            "https://example.invalid/source?version=1",
            "http://127.0.0.1/source",
            "file:///tmp/source",
        ] {
            validate_fetch_url(&Url::parse(accepted).unwrap()).unwrap();
        }

        for rejected in [
            "https://user:secret@example.invalid/source",
            "https://example.invalid/source#fragment",
            "file:///tmp/source?ignored=true",
            "data:text/plain,source",
        ] {
            let error = validate_fetch_url(&Url::parse(rejected).unwrap()).unwrap_err();
            assert!(matches!(error, Error::UrlPolicy { .. }));
            assert!(!error.to_string().contains("secret"));
            assert!(!error.to_string().contains(rejected));
        }
    }

    #[test]
    fn redirect_hop_limit_accepts_n_and_rejects_n_plus_one() {
        let target = Url::parse("https://example.invalid/source").unwrap();
        let ten = vec![target.clone(); 10];
        assert!(validate_redirect(&ten, &target).is_ok());

        let eleven = vec![target.clone(); 11];
        assert!(matches!(
            validate_redirect(&eleven, &target),
            Err(RedirectPolicyError::TooManyHops)
        ));
    }

    #[tokio::test]
    async fn exact_limit_is_admitted_but_n_plus_one_is_rejected_and_cleaned() {
        let directory = tempfile::tempdir().unwrap();
        let exact = directory.path().join("exact");
        download_from_source(
            std::future::ready(Ok(fetched(b"1234", Some(4)))),
            &exact,
            limits(4),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&exact).unwrap(), b"1234");

        let oversized = directory.path().join("oversized");
        let error = download_from_source(
            // A lying length hint proves the stream itself is still bounded.
            std::future::ready(Ok(fetched(b"12345", Some(4)))),
            &oversized,
            limits(4),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::TooLarge { limit: 4 }));
        assert!(!oversized.exists());
        assert_eq!(
            std::fs::read_dir(directory.path()).unwrap().count(),
            1,
            "only the successful exact-limit destination may remain"
        );
    }

    #[tokio::test]
    async fn announced_oversize_is_rejected_before_creating_staging() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("asset");
        let error = download_from_source(
            std::future::ready(Ok(fetched(b"", Some(5)))),
            &destination,
            limits(4),
            None,
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::TooLarge { limit: 4 }));
        assert!(std::fs::read_dir(directory.path()).unwrap().next().is_none());
    }

    struct PendingReader;

    impl AsyncRead for PendingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut task::Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> task::Poll<io::Result<()>> {
            task::Poll::Pending
        }
    }

    struct FailingReader;

    impl AsyncRead for FailingReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut task::Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> task::Poll<io::Result<()>> {
            task::Poll::Ready(Err(io::Error::other("synthetic read failure")))
        }
    }

    #[tokio::test]
    async fn stalled_reader_hits_total_timeout_and_removes_staging() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("asset");
        let started = Instant::now();
        let error = download_from_source(
            std::future::ready(Ok(Fetched {
                reader: Box::new(PendingReader),
                content_length: None,
            })),
            &destination,
            DownloadLimits::new(4, Duration::from_millis(25)),
            None,
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::Timeout { .. }));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(std::fs::read_dir(directory.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn reader_error_removes_staging_without_replacing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("asset");
        std::fs::write(&destination, b"previous complete asset").unwrap();
        let error = download_from_source(
            std::future::ready(Ok(Fetched {
                reader: Box::new(FailingReader),
                content_length: None,
            })),
            &destination,
            limits(64),
            None,
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::Read(_)));
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous complete asset");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn predictable_part_symlink_is_never_opened_or_replaced() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("asset");
        let legacy_partial = destination.with_added_extension("part");
        let victim = directory.path().join("victim");
        std::fs::write(&victim, b"do not overwrite").unwrap();
        symlink(&victim, &legacy_partial).unwrap();

        download_from_source(
            std::future::ready(Ok(fetched(b"downloaded", Some(10)))),
            &destination,
            limits(10),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"downloaded");
        assert_eq!(std::fs::read(&victim).unwrap(), b"do not overwrite");
        assert!(
            std::fs::symlink_metadata(legacy_partial)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn atomic_install_replaces_destination_symlink_without_touching_target() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("asset");
        let victim = directory.path().join("victim");
        std::fs::write(&victim, b"do not overwrite").unwrap();
        symlink(&victim, &destination).unwrap();

        download_from_source(
            std::future::ready(Ok(fetched(b"downloaded", Some(10)))),
            &destination,
            limits(10),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"downloaded");
        assert_eq!(std::fs::read(&victim).unwrap(), b"do not overwrite");
        assert!(std::fs::symlink_metadata(destination).unwrap().file_type().is_file());
    }

    #[tokio::test]
    async fn hash_mismatch_never_publishes_and_removes_staging() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("asset");
        std::fs::write(&destination, b"previous verified asset").unwrap();
        let error = download_from_source(
            std::future::ready(Ok(fetched(b"wrong bytes", Some(11)))),
            &destination,
            limits(11),
            None,
            Some(&hex::encode(Sha256::digest(b"locked bytes"))),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, Error::HashMismatch { .. }));
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous verified asset");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
