fn validate_regular_metadata(
    metadata: &std::fs::Metadata,
    exact_size: Option<u64>,
    max_size: u64,
    exact_mode: Option<u32>,
) -> io::Result<FileFingerprint> {
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache entry is not a regular file",
        ));
    }
    if metadata.uid() != nix::unistd::Uid::effective().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cache entry is not owned by the effective user",
        ));
    }
    if metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cache entry has {} links, expected exactly one", metadata.nlink()),
        ));
    }
    if metadata.len() > max_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cache entry is {} bytes, exceeding limit {max_size}", metadata.len()),
        ));
    }
    if exact_size.is_some_and(|expected| metadata.len() != expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "cache entry is {} bytes, expected exactly {}",
                metadata.len(),
                exact_size.unwrap()
            ),
        ));
    }
    let mode = metadata.mode() & 0o7777;
    if exact_mode.is_some_and(|expected| mode != expected) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cache entry mode is {mode:04o}, expected exactly {:04o}",
                exact_mode.unwrap()
            ),
        ));
    }
    Ok(FileFingerprint::from_metadata(metadata))
}

async fn authenticate_sha256_entry_async(
    directory: &Directory,
    name: &OsStr,
    expected_hash: &str,
    exact_size: Option<u64>,
    max_size: u64,
    exact_mode: Option<u32>,
) -> io::Result<std::fs::File> {
    let file = directory.open_regular(name)?;
    let fingerprint = authenticate_sha256_file_async(&file, expected_hash, exact_size, max_size, exact_mode).await?;
    let reopened = directory.open_regular(name)?;
    let reopened_fingerprint = validate_regular_metadata(&reopened.metadata()?, exact_size, max_size, exact_mode)?;
    if reopened_fingerprint != fingerprint {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache entry identity changed during authentication",
        ));
    }
    Ok(file)
}

async fn authenticate_sha256_file_async(
    file: &std::fs::File,
    expected_hash: &str,
    exact_size: Option<u64>,
    max_size: u64,
    exact_mode: Option<u32>,
) -> io::Result<FileFingerprint> {
    let before = validate_regular_metadata(&file.metadata()?, exact_size, max_size, exact_mode)?;
    let mut reader = tokio::fs::File::from_std(file.try_clone()?);
    reader.seek(io::SeekFrom::Start(0)).await?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut read_total = 0_u64;
    loop {
        let remaining = max_size.saturating_sub(read_total);
        let capacity = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = reader.read(&mut buffer[..capacity]).await?;
        if read == 0 {
            break;
        }
        if read as u64 > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cache entry stream exceeds limit {max_size}"),
            ));
        }
        hasher.update(&buffer[..read]);
        read_total += read as u64;
    }
    if exact_size.is_some_and(|expected| read_total != expected) {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "cache entry stream is {read_total} bytes, expected exactly {}",
                exact_size.unwrap()
            ),
        ));
    }
    let actual_hash = hex::encode(hasher.finalize());
    if actual_hash != expected_hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cache entry SHA-256 mismatch: expected {expected_hash}, got {actual_hash}"),
        ));
    }
    let after = validate_regular_metadata(&file.metadata()?, exact_size, max_size, exact_mode)?;
    if after != before {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache entry changed while its SHA-256 was computed",
        ));
    }
    Ok(after)
}

fn authenticate_sha256_file_sync(
    file: &mut std::fs::File,
    expected_hash: &str,
    exact_size: Option<u64>,
    max_size: u64,
    exact_mode: Option<u32>,
) -> io::Result<FileFingerprint> {
    use std::io::{Read as _, Seek as _};

    let before = validate_regular_metadata(&file.metadata()?, exact_size, max_size, exact_mode)?;
    file.seek(io::SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut read_total = 0_u64;
    loop {
        let remaining = max_size.saturating_sub(read_total);
        let capacity = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..capacity])?;
        if read == 0 {
            break;
        }
        if read as u64 > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cache entry stream exceeds limit {max_size}"),
            ));
        }
        hasher.update(&buffer[..read]);
        read_total += read as u64;
    }
    if exact_size.is_some_and(|expected| read_total != expected) {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "cache entry stream is {read_total} bytes, expected exactly {}",
                exact_size.unwrap()
            ),
        ));
    }
    let actual_hash = hex::encode(hasher.finalize());
    if actual_hash != expected_hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cache entry SHA-256 mismatch: expected {expected_hash}, got {actual_hash}"),
        ));
    }
    let after = validate_regular_metadata(&file.metadata()?, exact_size, max_size, exact_mode)?;
    if after != before {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache entry changed while its SHA-256 was computed",
        ));
    }
    file.seek(io::SeekFrom::Start(0))?;
    Ok(after)
}

fn authenticate_asset_entry(
    directory: &Directory,
    name: &OsStr,
    expected_digest: u128,
    expected_size: u64,
) -> io::Result<std::fs::File> {
    let mut file = directory.open_regular(name)?;
    let fingerprint = authenticate_asset_file(&mut file, expected_digest, expected_size)?;
    let reopened = directory.open_regular(name)?;
    let reopened_fingerprint = validate_regular_metadata(
        &reopened.metadata()?,
        Some(expected_size),
        expected_size,
        Some(PRIVATE_FILE_MODE),
    )?;
    if reopened_fingerprint != fingerprint {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "asset identity changed during authentication",
        ));
    }
    Ok(file)
}

fn authenticate_asset_file(
    file: &mut std::fs::File,
    expected_digest: u128,
    expected_size: u64,
) -> io::Result<FileFingerprint> {
    use std::io::{Read as _, Seek as _};

    let before = validate_regular_metadata(
        &file.metadata()?,
        Some(expected_size),
        expected_size,
        Some(PRIVATE_FILE_MODE),
    )?;
    file.seek(io::SeekFrom::Start(0))?;
    let mut hasher = StoneDigestWriterHasher::new();
    let mut digest_writer = StoneDigestWriter::new(io::sink(), &mut hasher);
    let copied = io::copy(&mut file.take(expected_size.saturating_add(1)), &mut digest_writer)?;
    if copied != expected_size {
        return Err(io::Error::new(
            if copied < expected_size {
                io::ErrorKind::UnexpectedEof
            } else {
                io::ErrorKind::InvalidData
            },
            format!("asset stream is {copied} bytes, expected exactly {expected_size}"),
        ));
    }
    let actual = hasher.digest128();
    if actual != expected_digest {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("asset digest mismatch: expected {expected_digest:02x}, got {actual:02x}"),
        ));
    }
    let after = validate_regular_metadata(
        &file.metadata()?,
        Some(expected_size),
        expected_size,
        Some(PRIVATE_FILE_MODE),
    )?;
    if after != before {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "asset changed while its digest was computed",
        ));
    }
    file.seek(io::SeekFrom::Start(0))?;
    Ok(after)
}

fn open_download_parent(installation: &Installation, hash: &str) -> io::Result<Directory> {
    validate_sha256(hash)?;
    let root = Directory::open_absolute(&installation.cache_path(""))?;
    let downloads = root.open_or_create_directory(OsStr::new("downloads"), CACHE_DIRECTORY_MODE)?;
    let version = downloads.open_or_create_directory(OsStr::new("v1"), CACHE_DIRECTORY_MODE)?;
    let prefix = version.open_or_create_directory(OsStr::new(&hash[..5]), CACHE_DIRECTORY_MODE)?;
    prefix.open_or_create_directory(OsStr::new(&hash[hash.len() - 5..]), CACHE_DIRECTORY_MODE)
}

fn open_asset_parent(root: &Directory, hash: &str) -> io::Result<(Directory, OsString)> {
    if hash.is_empty()
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "asset digest must be lowercase ASCII hexadecimal",
        ));
    }
    let mut parent = Directory {
        file: root.file.try_clone()?,
        path: root.path.clone(),
    };
    if hash.len() >= 10 {
        for component in [&hash[..2], &hash[2..4], &hash[4..6]] {
            parent = parent.open_or_create_directory(OsStr::new(component), CACHE_DIRECTORY_MODE)?;
        }
    }
    Ok((parent, OsString::from(hash)))
}

fn validate_sha256(hash: &str) -> io::Result<()> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "package hash must be exactly 64 lowercase ASCII hexadecimal characters",
        ));
    }
    Ok(())
}
