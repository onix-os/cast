fn scan_archive(
    source: &mut File,
    strip_components: usize,
    limits: ArchiveLimits,
    extraction_root: Option<&File>,
    source_date_epoch: i64,
    deadline: ArchiveDeadline,
) -> Result<ScanResult, Error> {
    deadline.checkpoint()?;
    source.seek(SeekFrom::Start(0))?;
    let decoded = decoded_reader(source, limits, deadline)?;
    let mut archive = Archive::new(decoded);
    archive.set_ignore_zeros(false);
    let mut entries = archive.entries()?.raw(true);
    let mut budget = ScanBudget::new(limits);
    let mut pending = PendingExtensions::default();
    let mut manifest = Vec::new();
    let mut topology = BTreeMap::<Vec<Vec<u8>>, ManifestKind>::new();
    let mut materialization = MaterializationTrie::default();

    while let Some(entry) = entries.next() {
        deadline.checkpoint()?;
        let mut entry = entry?;
        let entry_index = budget.entry()?;
        let entry_type = entry.header().entry_type();

        if entry_type.is_gnu_sparse() {
            return Err(Error::SparseEntry { entry: entry_index });
        }
        if entry_type.is_pax_global_extensions() {
            return Err(Error::GlobalPaxEntry { entry: entry_index });
        }
        if entry_type.is_gnu_longname() {
            if pending.path.is_some() {
                return Err(Error::DuplicateExtension {
                    entry: entry_index,
                    field: "path",
                });
            }
            pending.path = Some(read_extension_bytes(&mut entry, entry_index, &mut budget)?);
            continue;
        }
        if entry_type.is_gnu_longlink() {
            if pending.link.is_some() {
                return Err(Error::DuplicateExtension {
                    entry: entry_index,
                    field: "linkpath",
                });
            }
            pending.link = Some(read_extension_bytes(&mut entry, entry_index, &mut budget)?);
            continue;
        }
        if entry_type.is_pax_local_extensions() {
            read_pax_extensions(&mut entry, entry_index, &mut budget, &mut pending)?;
            continue;
        }

        let raw_path = pending.path.take().unwrap_or_else(|| entry.path_bytes().into_owned());
        let raw_link = pending
            .link
            .take()
            .or_else(|| entry.link_name_bytes().map(|value| value.into_owned()));
        budget.paths(raw_path.len())?;

        let kind = classify_entry(entry_type, entry_index)?;
        let original = canonical_relative_components(&raw_path, kind == ManifestKind::Directory, entry_index, limits)?;
        let path = original.get(strip_components..).unwrap_or(&[]).to_vec();
        let mode = entry.header().mode()? & 0o777;
        let logical_bytes = entry.size();
        let physical_bytes = entry.size();
        budget.data(logical_bytes, physical_bytes)?;

        let link_target = match kind {
            ManifestKind::Symlink => {
                if logical_bytes != 0 {
                    return Err(Error::DataOnNonRegular { entry: entry_index });
                }
                let raw_link = require_link(raw_link, entry_index)?;
                budget.paths(raw_link.len())?;
                validate_symlink_target(&original, &raw_link, entry_index, limits)?;
                if !path.is_empty() {
                    validate_symlink_target(&path, &raw_link, entry_index, limits)?;
                }
                Some(raw_link)
            }
            ManifestKind::Hardlink => {
                if logical_bytes != 0 {
                    return Err(Error::DataOnNonRegular { entry: entry_index });
                }
                let raw_link = require_link(raw_link, entry_index)?;
                budget.paths(raw_link.len())?;
                let original_target = canonical_relative_components(&raw_link, false, entry_index, limits)?;
                let target = original_target.get(strip_components..).unwrap_or(&[]).to_vec();
                if !path.is_empty() && target.is_empty() {
                    return Err(Error::StrippedHardlinkTarget { entry: entry_index });
                }
                Some(join_components(&target))
            }
            ManifestKind::Regular | ManifestKind::Directory => {
                if raw_link.is_some() {
                    return Err(Error::UnexpectedLinkTarget { entry: entry_index });
                }
                if kind == ManifestKind::Directory && logical_bytes != 0 {
                    return Err(Error::DataOnNonRegular { entry: entry_index });
                }
                None
            }
        };

        let hardlink_target = if path.is_empty() || kind != ManifestKind::Hardlink {
            None
        } else {
            Some(split_joined_components(link_target.as_deref().unwrap_or_default()))
        };
        if !path.is_empty() {
            validate_topology(&topology, &path, kind, entry_index)?;
            materialization.admit(&path, kind, mode, limits.materialized_nodes)?;
            if let Some(target) = &hardlink_target {
                let Some(target_kind) = topology.get(target) else {
                    return Err(Error::ForwardHardlink { entry: entry_index });
                };
                if !matches!(target_kind, ManifestKind::Regular | ManifestKind::Hardlink) {
                    return Err(Error::InvalidHardlinkTargetType { entry: entry_index });
                }
            }
        }

        let mut digest = None;
        if kind == ManifestKind::Regular {
            let mut hasher = Sha256::new();
            let mut writer = extraction_root
                .filter(|_| !path.is_empty())
                .map(|root| create_regular_beneath(root, &path, source_date_epoch))
                .transpose()?;
            let copied = copy_entry(&mut entry, writer.as_mut(), &mut hasher, logical_bytes, deadline)?;
            if copied != logical_bytes {
                return Err(Error::EntrySizeMismatch {
                    entry: entry_index,
                    expected: logical_bytes,
                    found: copied,
                });
            }
            if let Some(file) = writer {
                set_file_mode_and_time(&file, mode, source_date_epoch)?;
                file.sync_all()?;
            }
            digest = Some(hasher.finalize().into());
        } else {
            ensure_entry_consumed(&mut entry, entry_index)?;
        }

        if path.is_empty() {
            continue;
        }
        if kind == ManifestKind::Hardlink {
            let target = hardlink_target.as_deref().expect("validated hardlink target");
            if let Some(root) = extraction_root {
                create_hardlink_beneath(root, target, &path)?;
            }
        } else if kind == ManifestKind::Symlink {
            if let Some(root) = extraction_root {
                create_symlink_beneath(
                    root,
                    link_target.as_deref().unwrap_or_default(),
                    &path,
                    source_date_epoch,
                )?;
            }
        } else if kind == ManifestKind::Directory {
            if let Some(root) = extraction_root {
                ensure_directories(root, &path)?;
            }
        }
        topology.insert(path.clone(), kind);
        manifest.push(ManifestEntry {
            path,
            kind,
            mode,
            logical_bytes,
            physical_bytes,
            link_target,
            sha256: digest,
        });
    }

    if !pending.is_empty() {
        return Err(Error::DanglingExtension);
    }
    drop(entries);
    let mut decoded = archive.into_inner();
    io::copy(&mut decoded, &mut io::sink())?;
    let decoded_bytes = decoded.consumed;

    if let Some(root) = extraction_root {
        normalize_materialized_directories(
            root,
            &materialization.root,
            &mut Vec::new(),
            source_date_epoch,
            deadline,
        )?;
        set_file_mode_and_time(root, 0o755, source_date_epoch)?;
        root.sync_all()?;
    }
    Ok(ScanResult {
        manifest,
        usage: ScanUsage {
            decoded_bytes,
            entries: budget.entries,
            path_bytes: budget.path_bytes,
            extension_bytes: budget.extension_bytes,
            logical_bytes: budget.total_logical_bytes,
            physical_bytes: budget.total_physical_bytes,
            materialized_nodes: materialization.nodes,
        },
    })
}

fn normalize_materialized_directories(
    root: &File,
    node: &MaterializationNode,
    path: &mut Vec<Vec<u8>>,
    source_date_epoch: i64,
    deadline: ArchiveDeadline,
) -> Result<(), Error> {
    for (component, child) in &node.children {
        deadline.checkpoint()?;
        path.push(component.clone());
        normalize_materialized_directories(root, child, path, source_date_epoch, deadline)?;
        if let Some(mode) = child.directory_mode {
            let directory = open_directory_beneath(root, path, "extracted directory")?;
            set_file_mode_and_time(&directory, mode, source_date_epoch)?;
            directory.sync_all()?;
        }
        path.pop();
    }
    Ok(())
}

fn decoded_reader<'a>(
    source: &'a mut File,
    limits: ArchiveLimits,
    deadline: ArchiveDeadline,
) -> Result<DecodedLimit<Box<dyn Read + 'a>>, Error> {
    deadline.checkpoint()?;
    let mut magic = [0u8; 8];
    let found = source.read(&mut magic)?;
    source.seek(SeekFrom::Start(0))?;
    let input = source.take(limits.compressed_bytes.saturating_add(1));
    let decoded: Box<dyn Read + 'a> = if magic[..found].starts_with(&[0x1f, 0x8b]) {
        Box::new(MultiGzDecoder::new(input))
    } else if magic[..found].starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        let stream = XzStream::new_stream_decoder(limits.xz_decoder_memory_bytes, XZ_CONCATENATED)
            .map_err(|source| io::Error::new(io::ErrorKind::InvalidInput, source))?;
        Box::new(XzDecoder::new_stream(input, stream))
    } else if magic[..found].starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        let mut decoder = zstd::stream::read::Decoder::new(input)?;
        decoder.window_log_max(limits.zstd_window_log_max)?;
        Box::new(decoder)
    } else if unsupported_compression_magic(&magic[..found]) {
        return Err(Error::UnsupportedArchiveCompression);
    } else {
        Box::new(input)
    };
    Ok(DecodedLimit::new(decoded, limits.decoded_bytes, deadline))
}

fn unsupported_compression_magic(magic: &[u8]) -> bool {
    magic.starts_with(b"BZh")
        || magic.starts_with(b"PK\x03\x04")
        || magic.starts_with(b"7z\xbc\xaf\x27\x1c")
        || magic.starts_with(b"Rar!\x1a\x07")
        || magic.starts_with(b"LZIP")
        || magic.starts_with(&[0x1f, 0x9d])
}

struct DecodedLimit<R> {
    inner: R,
    remaining: u64,
    consumed: u64,
    exhausted: bool,
    deadline: ArchiveDeadline,
}

impl<R> DecodedLimit<R> {
    fn new(inner: R, limit: u64, deadline: ArchiveDeadline) -> Self {
        Self {
            inner,
            remaining: limit,
            consumed: 0,
            exhausted: false,
            deadline,
        }
    }
}

impl<R: Read> Read for DecodedLimit<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.deadline.checkpoint_io()?;
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            if self.exhausted {
                return Ok(0);
            }
            let mut probe = [0u8; 1];
            let found = self.inner.read(&mut probe)?;
            self.deadline.checkpoint_io()?;
            if found == 0 {
                self.exhausted = true;
                return Ok(0);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "decoded archive byte limit exceeded",
            ));
        }
        let allowed = usize::try_from(self.remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let found = self.inner.read(&mut buffer[..allowed])?;
        self.deadline.checkpoint_io()?;
        self.remaining -= found as u64;
        self.consumed = self
            .consumed
            .checked_add(found as u64)
            .ok_or_else(|| io::Error::other("decoded archive byte counter overflowed"))?;
        Ok(found)
    }
}

fn read_extension_bytes<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    entry_index: u64,
    budget: &mut ScanBudget,
) -> Result<Vec<u8>, Error> {
    budget.extension(entry.size())?;
    let mut value = Vec::with_capacity(usize::try_from(entry.size()).map_err(|_| Error::ArithmeticOverflow)?);
    entry.read_to_end(&mut value)?;
    if value.last() == Some(&0) {
        value.pop();
    }
    if value.is_empty() || value.contains(&0) {
        return Err(Error::InvalidExtensionValue { entry: entry_index });
    }
    Ok(value)
}

fn read_pax_extensions<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    entry_index: u64,
    budget: &mut ScanBudget,
    pending: &mut PendingExtensions,
) -> Result<(), Error> {
    budget.extension(entry.size())?;
    let extensions = entry
        .pax_extensions()?
        .ok_or(Error::InvalidPaxEntry { entry: entry_index })?;
    let mut keys = BTreeSet::new();
    for extension in extensions {
        let extension = extension?;
        let key = extension.key_bytes();
        if !keys.insert(key.to_vec()) {
            return Err(Error::DuplicatePaxKey { entry: entry_index });
        }
        if key.starts_with(b"GNU.sparse.") {
            return Err(Error::SparseEntry { entry: entry_index });
        }
        match key {
            b"path" => {
                if pending.path.replace(extension.value_bytes().to_vec()).is_some() {
                    return Err(Error::DuplicateExtension {
                        entry: entry_index,
                        field: "path",
                    });
                }
            }
            b"linkpath" => {
                if pending.link.replace(extension.value_bytes().to_vec()).is_some() {
                    return Err(Error::DuplicateExtension {
                        entry: entry_index,
                        field: "linkpath",
                    });
                }
            }
            // These fields do not affect the extracted byte graph.  Ownership,
            // archive timestamps, and comments are intentionally discarded.
            b"uid" | b"gid" | b"uname" | b"gname" | b"mtime" | b"atime" | b"ctime" | b"charset" | b"comment" => {}
            _ => return Err(Error::UnsupportedPaxKey { entry: entry_index }),
        }
    }
    Ok(())
}

fn classify_entry(entry_type: EntryType, entry: u64) -> Result<ManifestKind, Error> {
    if entry_type.is_file() {
        Ok(ManifestKind::Regular)
    } else if entry_type.is_dir() {
        Ok(ManifestKind::Directory)
    } else if entry_type.is_symlink() {
        Ok(ManifestKind::Symlink)
    } else if entry_type.is_hard_link() {
        Ok(ManifestKind::Hardlink)
    } else {
        Err(Error::UnsupportedInodeType { entry })
    }
}

fn canonical_relative_components(
    path: &[u8],
    allow_trailing_slash: bool,
    entry: u64,
    limits: ArchiveLimits,
) -> Result<Vec<Vec<u8>>, Error> {
    if path.is_empty() || path[0] == b'/' || path.contains(&0) || path.contains(&b'\\') {
        return Err(Error::UnsafePath { entry });
    }
    require_usize_limit("one archive path bytes", path.len(), limits.one_path_bytes)?;
    let mut raw = path.split(|byte| *byte == b'/').collect::<Vec<_>>();
    if allow_trailing_slash && raw.last().is_some_and(|component| component.is_empty()) {
        raw.pop();
    }
    if raw.is_empty()
        || raw
            .iter()
            .any(|component| component.is_empty() || *component == b"." || *component == b"..")
    {
        return Err(Error::UnsafePath { entry });
    }
    require_usize_limit("archive path depth", raw.len(), limits.path_depth)?;
    Ok(raw.into_iter().map(<[u8]>::to_vec).collect())
}

fn validate_symlink_target(
    link_path: &[Vec<u8>],
    target: &[u8],
    entry: u64,
    limits: ArchiveLimits,
) -> Result<(), Error> {
    if target.is_empty() || target[0] == b'/' || target.contains(&0) || target.contains(&b'\\') {
        return Err(Error::EscapingSymlink { entry });
    }
    require_usize_limit("archive link bytes", target.len(), limits.link_bytes)?;
    let mut resolved = link_path[..link_path.len().saturating_sub(1)].to_vec();
    for component in target.split(|byte| *byte == b'/') {
        match component {
            b"" => return Err(Error::EscapingSymlink { entry }),
            b"." => {}
            b".." => {
                if resolved.pop().is_none() {
                    return Err(Error::EscapingSymlink { entry });
                }
            }
            value => resolved.push(value.to_vec()),
        }
        require_usize_limit("archive link depth", resolved.len(), limits.path_depth)?;
    }
    Ok(())
}

fn require_link(link: Option<Vec<u8>>, entry: u64) -> Result<Vec<u8>, Error> {
    link.filter(|value| !value.is_empty())
        .ok_or(Error::MissingLinkTarget { entry })
}

fn validate_topology(
    topology: &BTreeMap<Vec<Vec<u8>>, ManifestKind>,
    path: &[Vec<u8>],
    kind: ManifestKind,
    entry: u64,
) -> Result<(), Error> {
    if topology.contains_key(path) {
        return Err(Error::DuplicatePath { entry });
    }
    for depth in 1..path.len() {
        if let Some(parent_kind) = topology.get(&path[..depth])
            && *parent_kind != ManifestKind::Directory
        {
            return Err(Error::PathTypeCollision { entry });
        }
    }
    if kind != ManifestKind::Directory {
        let next = topology
            .range((std::ops::Bound::Excluded(path.to_vec()), std::ops::Bound::Unbounded))
            .next()
            .map(|(path, _)| path);
        if next.is_some_and(|other| other.len() > path.len() && other.starts_with(path)) {
            return Err(Error::PathTypeCollision { entry });
        }
    }
    Ok(())
}

fn copy_entry<R: Read>(
    entry: &mut R,
    mut output: Option<&mut File>,
    hasher: &mut Sha256,
    limit: u64,
    deadline: ArchiveDeadline,
) -> Result<u64, Error> {
    let mut total = 0u64;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        deadline.checkpoint()?;
        let found = entry.read(&mut buffer)?;
        deadline.checkpoint()?;
        if found == 0 {
            break;
        }
        total = total
            .checked_add(u64::try_from(found).map_err(|_| Error::ArithmeticOverflow)?)
            .ok_or(Error::ArithmeticOverflow)?;
        if total > limit {
            return Err(Error::EntryStreamExceededDeclaredSize);
        }
        hasher.update(&buffer[..found]);
        if let Some(output) = output.as_deref_mut() {
            output.write_all(&buffer[..found])?;
        }
    }
    Ok(total)
}

fn ensure_entry_consumed<R: Read>(entry: &mut R, index: u64) -> Result<(), Error> {
    let mut buffer = [0u8; 1];
    if entry.read(&mut buffer)? == 0 {
        Ok(())
    } else {
        Err(Error::DataOnNonRegular { entry: index })
    }
}

fn require_archive_digest(
    source: &mut File,
    expected: &str,
    limit: u64,
    deadline: ArchiveDeadline,
) -> Result<(), Error> {
    source.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    loop {
        deadline.checkpoint()?;
        let found = source.read(&mut buffer)?;
        deadline.checkpoint()?;
        if found == 0 {
            break;
        }
        total = total.checked_add(found as u64).ok_or(Error::ArithmeticOverflow)?;
        require_limit("compressed archive bytes", total, limit)?;
        hasher.update(&buffer[..found]);
    }
    let found = hex::encode(hasher.finalize());
    if found == expected {
        source.seek(SeekFrom::Start(0))?;
        Ok(())
    } else {
        Err(Error::ArchiveDigestMismatch)
    }
}
