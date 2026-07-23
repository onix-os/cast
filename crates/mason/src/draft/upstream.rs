// SPDX-FileCopyrightText: 2024 AerynOS Developers

use std::{io, path::Path, time::Duration};

use forge::{request, runtime};
use tempfile::NamedTempFile;
use thiserror::Error;
use tui::{MultiProgress, ProgressBar, ProgressStyle, Styled};
use url::Url;

use stone_recipe::{UpstreamSpec, spec::UpstreamValidationError};

use crate::{
    Env,
    archive::{self, ArchiveSessionBudget},
    upstream::ARCHIVE_DOWNLOAD_LIMITS,
};

use super::File;

pub struct Upstream {
    pub uri: Url,
    pub hash: String,
}

pub struct Extracted {
    pub upstreams: Vec<Upstream>,
    pub files: Vec<File>,
}

/// Fetch and extract archive inputs sequentially in their authored order.
///
/// Sequential admission is deliberate: it makes one aggregate extraction
/// budget authoritative without reserving bytes speculatively across tasks.
pub fn fetch_and_extract(
    env: &Env,
    upstreams: &[Url],
    download_root: &Path,
    extract_root: &Path,
) -> Result<Extracted, Error> {
    preflight_upstreams(upstreams)?;
    let progress = MultiProgress::new();
    let result = runtime::block_on(fetch_and_extract_sequential(
        env,
        upstreams,
        download_root,
        extract_root,
        &progress,
    ));
    println!();
    result
}

fn preflight_upstreams(upstreams: &[Url]) -> Result<(), Error> {
    if upstreams.len() > ArchiveSessionBudget::maximum_extractions() {
        return Err(Error::TooManyUpstreams {
            actual: upstreams.len(),
            limit: ArchiveSessionBudget::maximum_extractions(),
        });
    }
    for (index, uri) in upstreams.iter().enumerate() {
        draft_spec(uri, "0".repeat(64))
            .validate()
            .map_err(|source| Error::InvalidUpstream { index, source })?;
    }
    Ok(())
}

async fn fetch_and_extract_sequential(
    env: &Env,
    upstreams: &[Url],
    download_root: &Path,
    extract_root: &Path,
    progress: &MultiProgress,
) -> Result<Extracted, Error> {
    let mut session = ArchiveSessionBudget::production();
    let mut extracted = Extracted {
        upstreams: Vec::with_capacity(upstreams.len()),
        files: Vec::new(),
    };

    for (index, uri) in upstreams.iter().enumerate() {
        let remaining_bytes = session.remaining_compressed_bytes()?;
        let remaining_time = session.remaining_wall_time()?;
        let limits = request::DownloadLimits::new(
            ARCHIVE_DOWNLOAD_LIMITS.max_bytes.min(remaining_bytes),
            ARCHIVE_DOWNLOAD_LIMITS.total_timeout.min(remaining_time),
        );
        let temporary = NamedTempFile::with_prefix_in("cast-draft-", download_root)?;
        let temporary_path = temporary.path().to_owned();
        let temporary_name = temporary_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Error::InvalidTemporaryName)?;
        let bar = download_bar(progress, uri);
        let hash = request::download_with_sha256_and_limits(uri.clone(), &temporary_path, limits).await?;

        bar.set_message(format!("{} {}", "Extracting".yellow(), uri));
        let destination = format!("upstream-{index:04}");
        extracted.files.extend(materialize_downloaded_archive(
            download_root,
            temporary_name,
            &hash,
            extract_root,
            &destination,
            &mut session,
        )?);

        crate::upstream::admit_downloaded_archive(
            &env.cache_dir.join("upstreams"),
            uri.clone(),
            &hash,
            &temporary_path,
            index,
        )?;
        extracted.upstreams.push(Upstream { uri: uri.clone(), hash });
        bar.suspend(|| println!("{} {}", "Fetched".green(), uri));
        progress.remove(&bar);
    }

    Ok(extracted)
}

fn materialize_downloaded_archive(
    download_root: &Path,
    source_name: &str,
    hash: &str,
    extract_root: &Path,
    destination: &str,
    session: &mut ArchiveSessionBudget,
) -> Result<Vec<File>, Error> {
    let manifest = archive::extract_draft_tar(download_root, source_name, hash, extract_root, destination, session)?;
    let destination_root = extract_root.join(destination);
    Ok(manifest
        .into_files()
        .into_iter()
        .map(|relative| {
            let depth = relative.iter().count().saturating_sub(2);
            File::new(destination_root.join(relative), depth)
        })
        .collect())
}

fn draft_spec(uri: &Url, hash: String) -> UpstreamSpec {
    UpstreamSpec::Archive {
        url: uri.to_string(),
        hash,
        rename: None,
        strip_dirs: None,
        unpack: true,
        unpack_dir: None,
    }
}

fn download_bar(progress: &MultiProgress, uri: &Url) -> ProgressBar {
    let bar = progress.add(
        ProgressBar::new_spinner()
            .with_style(
                ProgressStyle::with_template(" {spinner} {wide_msg}")
                    .unwrap()
                    .tick_chars("--=≡■≡=--"),
            )
            .with_message(format!("{} {}", "Downloading".blue(), uri)),
    );
    bar.enable_steady_tick(Duration::from_millis(150));
    bar
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("received {actual} source archives; the drafting limit is {limit}")]
    TooManyUpstreams { actual: usize, limit: usize },
    #[error("source archive {index} is invalid: {source}")]
    InvalidUpstream {
        index: usize,
        #[source]
        source: UpstreamValidationError,
    },
    #[error("temporary archive name is not valid UTF-8")]
    InvalidTemporaryName,
    #[error("bounded archive extraction")]
    Archive(#[from] archive::Error),
    #[error("admit downloaded archive to cache")]
    Cache(#[from] crate::upstream::Error),
    #[error("download source archive")]
    Request(#[from] request::Error),
    #[error("I/O while drafting source archive")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Cursor};

    use sha2::{Digest, Sha256};
    use tar::{Builder, EntryType, Header};

    use super::*;

    fn archive(build: impl FnOnce(&mut Builder<Vec<u8>>)) -> Vec<u8> {
        let mut builder = Builder::new(Vec::new());
        build(&mut builder);
        builder.finish().unwrap();
        builder.into_inner().unwrap()
    }

    fn append(builder: &mut Builder<Vec<u8>>, path: &str, kind: EntryType, link: Option<&str>, data: &[u8]) {
        let mut header = Header::new_ustar();
        header.set_path(path).unwrap();
        header.set_entry_type(kind);
        header.set_mode(if kind.is_dir() { 0o755 } else { 0o644 });
        header.set_size(data.len() as u64);
        if let Some(link) = link {
            header.set_link_name(link).unwrap();
        }
        header.set_cksum();
        builder.append(&header, Cursor::new(data)).unwrap();
    }

    fn write_archive(root: &Path, name: &str, bytes: &[u8]) -> String {
        fs::write(root.join(name), bytes).unwrap();
        hex::encode(Sha256::digest(bytes))
    }

    #[test]
    fn same_named_files_from_multiple_inputs_remain_isolated_and_ordered() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        let extracted = root.path().join("extracted");
        fs::create_dir(&downloads).unwrap();
        fs::create_dir(&extracted).unwrap();
        let first = archive(|builder| append(builder, "project/CMakeLists.txt", EntryType::Regular, None, b"first"));
        let second = archive(|builder| append(builder, "project/CMakeLists.txt", EntryType::Regular, None, b"second"));
        let first_hash = write_archive(&downloads, "first.tar", &first);
        let second_hash = write_archive(&downloads, "second.tar", &second);
        let mut session = ArchiveSessionBudget::production();

        let mut files = materialize_downloaded_archive(
            &downloads,
            "first.tar",
            &first_hash,
            &extracted,
            "upstream-0000",
            &mut session,
        )
        .unwrap();
        files.extend(
            materialize_downloaded_archive(
                &downloads,
                "second.tar",
                &second_hash,
                &extracted,
                "upstream-0001",
                &mut session,
            )
            .unwrap(),
        );

        assert_eq!(files.len(), 2);
        assert!(files[0].path.starts_with(extracted.join("upstream-0000")));
        assert!(files[1].path.starts_with(extracted.join("upstream-0001")));
        assert_eq!(fs::read(&files[0].path).unwrap(), b"first");
        assert_eq!(fs::read(&files[1].path).unwrap(), b"second");
        assert_eq!(files[0].depth(), 0);
        assert_eq!(files[1].depth(), 0);
    }

    #[test]
    fn manifest_file_list_never_follows_an_extracted_ancestor_symlink() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        let extracted = root.path().join("extracted");
        fs::create_dir(&downloads).unwrap();
        fs::create_dir(&extracted).unwrap();
        let bytes = archive(|builder| {
            append(builder, "project/CMakeLists.txt", EntryType::Regular, None, b"safe");
            append(builder, "project/loop", EntryType::Symlink, Some(".."), b"");
        });
        let hash = write_archive(&downloads, "source.tar", &bytes);
        let mut session = ArchiveSessionBudget::production();

        let files = materialize_downloaded_archive(
            &downloads,
            "source.tar",
            &hash,
            &extracted,
            "upstream-0000",
            &mut session,
        )
        .unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name(), "CMakeLists.txt");
        assert!(
            fs::symlink_metadata(extracted.join("upstream-0000/project/loop"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn unsupported_archive_type_and_digest_mismatch_publish_no_tree() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        let extracted = root.path().join("extracted");
        fs::create_dir(&downloads).unwrap();
        fs::create_dir(&extracted).unwrap();
        let special = archive(|builder| append(builder, "project/pipe", EntryType::Fifo, None, b""));
        let special_hash = write_archive(&downloads, "special.tar", &special);
        let ordinary = archive(|builder| append(builder, "project/file", EntryType::Regular, None, b"bytes"));
        write_archive(&downloads, "ordinary.tar", &ordinary);
        let mut session = ArchiveSessionBudget::production();

        assert!(matches!(
            materialize_downloaded_archive(
                &downloads,
                "special.tar",
                &special_hash,
                &extracted,
                "special",
                &mut session,
            ),
            Err(Error::Archive(archive::Error::UnsupportedInodeType { .. }))
        ));
        assert!(!extracted.join("special").exists());
        assert!(matches!(
            materialize_downloaded_archive(
                &downloads,
                "ordinary.tar",
                &"0".repeat(64),
                &extracted,
                "mismatch",
                &mut session,
            ),
            Err(Error::Archive(archive::Error::ArchiveDigestMismatch))
        ));
        assert!(!extracted.join("mismatch").exists());
    }

    #[test]
    fn source_count_and_transport_are_rejected_by_preflight() {
        let too_many = (0..=ArchiveSessionBudget::maximum_extractions())
            .map(|index| Url::parse(&format!("https://example.invalid/source-{index}.tar")).unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(
            preflight_upstreams(&too_many),
            Err(Error::TooManyUpstreams { .. })
        ));
        assert!(matches!(
            preflight_upstreams(&[Url::parse("http://example.invalid/source.tar").unwrap()]),
            Err(Error::InvalidUpstream { index: 0, .. })
        ));
    }
}
