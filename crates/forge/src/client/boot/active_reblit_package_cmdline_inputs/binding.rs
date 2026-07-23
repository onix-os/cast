use std::{
    os::fd::{AsRawFd as _, BorrowedFd},
    os::unix::ffi::OsStrExt as _,
};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PackageCmdlineCheckpoint {
    SourceAuthenticated { binding_index: usize },
    PreparedValueMaterialized,
}

pub(super) fn prepare_with_policy_until<'stone>(
    stone: &'stone PreparedActiveReblitStoneBootInputs,
    policy: PackageCmdlinePolicy,
    deadline: Instant,
) -> Result<PreparedActiveReblitPackageCmdlineInputs<'stone>, ActiveReblitPackageCmdlineInputsError> {
    prepare_with_policy_until_and_checkpoint(stone, policy, deadline, |_| {})
}

pub(super) fn prepare_with_policy_until_and_checkpoint<'stone, F>(
    stone: &'stone PreparedActiveReblitStoneBootInputs,
    policy: PackageCmdlinePolicy,
    deadline: Instant,
    mut checkpoint: F,
) -> Result<PreparedActiveReblitPackageCmdlineInputs<'stone>, ActiveReblitPackageCmdlineInputsError>
where
    F: FnMut(PackageCmdlineCheckpoint),
{
    let mut budget = PackageCmdlineBudget::new(policy, deadline)?;
    let projected_state_ids = copy_state_ids(stone.state_ids())?;
    let asset_count = stone.assets().len();
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(asset_count.min(policy.max_entries))
        .map_err(|source| ActiveReblitPackageCmdlineInputsError::Allocation {
            resource: "package command-line entries",
            source,
        })?;

    for (binding_index, asset) in stone.assets().enumerate() {
        budget.step("Stone asset inventory")?;
        if !matches!(
            asset.role(),
            BootAssetRole::GlobalCmdline | BootAssetRole::KernelCmdline { .. }
        ) {
            continue;
        }
        let actual = entries.len().saturating_add(1);
        if actual > policy.max_entries {
            return Err(ActiveReblitPackageCmdlineInputsError::EntryCountLimit {
                limit: policy.max_entries,
                actual,
            });
        }
        let state_position =
            validated_state_position(&projected_state_ids, asset.state_id(), binding_index, &mut budget)?;
        let entry = prepare_entry(binding_index, state_position, asset, &mut budget)?;
        checkpoint(PackageCmdlineCheckpoint::SourceAuthenticated { binding_index });
        budget.require_deadline("source authentication checkpoint")?;
        entries.push(entry);
    }

    budget.reserve_sort_work(entries.len())?;
    entries.sort_by(|left, right| {
        (left.state_position, &left.logical_path, left.binding_index).cmp(&(
            right.state_position,
            &right.logical_path,
            right.binding_index,
        ))
    });
    let prepared = PreparedActiveReblitPackageCmdlineInputs {
        source_owner: stone,
        projected_state_ids,
        entries: entries.into_boxed_slice(),
        total_source_bytes: budget.source_bytes,
        preparation_work: budget.work,
    };
    checkpoint(PackageCmdlineCheckpoint::PreparedValueMaterialized);
    budget.require_deadline("terminal package command-line checkpoint")?;
    Ok(prepared)
}

pub(super) fn revalidate_with_policy_until(
    prepared: &PreparedActiveReblitPackageCmdlineInputs<'_>,
    policy: PackageCmdlinePolicy,
    deadline: Instant,
) -> Result<(), ActiveReblitPackageCmdlineInputsError> {
    let mut budget = PackageCmdlineBudget::new(policy, deadline)?;
    if prepared.source_owner.state_ids() != prepared.projected_state_ids.as_ref() {
        return Err(ActiveReblitPackageCmdlineInputsError::StateProjectionChanged);
    }
    if prepared.entries.len() > policy.max_entries {
        return Err(ActiveReblitPackageCmdlineInputsError::EntryCountLimit {
            limit: policy.max_entries,
            actual: prepared.entries.len(),
        });
    }

    for expected in &prepared.entries {
        budget.step("retained source coordinate lookup")?;
        let binding_index = usize::from(expected.binding_index);
        let Some(asset) = prepared.source_owner.asset_at(binding_index) else {
            return Err(ActiveReblitPackageCmdlineInputsError::SourceChanged { binding_index });
        };
        if prepared.projected_state_ids.get(usize::from(expected.state_position)) != Some(&expected.state_id)
            || !coordinate_matches(expected, &asset)
        {
            return Err(ActiveReblitPackageCmdlineInputsError::SourceChanged { binding_index });
        }
        let length = budget.admit_source(binding_index, asset.length())?;
        let bytes = read_exact_source_at(asset.descriptor(), length, binding_index, &mut budget)?;
        require_digest(binding_index, asset.digest(), &bytes)?;
        let normalized = normalization::normalize_package_cmdline(binding_index, &bytes, &mut budget)?;
        if normalized.as_ref() != expected.snippet.as_ref() {
            return Err(ActiveReblitPackageCmdlineInputsError::SourceChanged { binding_index });
        }
    }

    if budget.source_bytes != prepared.total_source_bytes {
        return Err(ActiveReblitPackageCmdlineInputsError::SourceChanged {
            binding_index: prepared
                .entries
                .first()
                .map_or(0, |entry| usize::from(entry.binding_index)),
        });
    }
    budget.require_deadline("terminal source revalidation checkpoint")
}

fn prepare_entry(
    binding_index: usize,
    state_position: u16,
    asset: BoundActiveReblitBootAsset<'_>,
    budget: &mut PackageCmdlineBudget,
) -> Result<PreparedActiveReblitPackageCmdline, ActiveReblitPackageCmdlineInputsError> {
    let binding_index_u16 =
        u16::try_from(binding_index).map_err(|_| ActiveReblitPackageCmdlineInputsError::BindingIndexLimit {
            limit: u16::MAX as usize,
            actual: binding_index,
        })?;
    let scope = validated_scope(binding_index, &asset)?;
    let logical_path = clone_path(asset.logical_path(), "package command-line logical path")?;
    let filename = clone_os_string(
        asset
            .logical_path()
            .file_name()
            .ok_or(ActiveReblitPackageCmdlineInputsError::InvalidCoordinate { binding_index })?,
        "package command-line filename",
    )?;
    let length = budget.admit_source(binding_index, asset.length())?;
    let bytes = read_exact_source_at(asset.descriptor(), length, binding_index, budget)?;
    require_digest(binding_index, asset.digest(), &bytes)?;
    let snippet = normalization::normalize_package_cmdline(binding_index, &bytes, budget)?;

    Ok(PreparedActiveReblitPackageCmdline {
        state_id: asset.state_id(),
        state_position,
        scope,
        logical_path,
        filename,
        snippet,
        binding_index: binding_index_u16,
        digest: asset.digest(),
        length: asset.length(),
    })
}

fn validated_scope(
    binding_index: usize,
    asset: &BoundActiveReblitBootAsset<'_>,
) -> Result<PackageCmdlineScope, ActiveReblitPackageCmdlineInputsError> {
    let logical = asset.logical_path();
    let valid_filename = logical.file_name().is_some_and(|filename| {
        filename.as_bytes().len() > b".cmdline".len() && filename.as_bytes().ends_with(b".cmdline")
    });
    if !valid_filename {
        return Err(ActiveReblitPackageCmdlineInputsError::InvalidCoordinate { binding_index });
    }

    match asset.role() {
        BootAssetRole::GlobalCmdline => {
            let relative = logical
                .strip_prefix(Path::new("/usr/lib/kernel/cmdline.d"))
                .map_err(|_| ActiveReblitPackageCmdlineInputsError::InvalidCoordinate { binding_index })?;
            if relative.components().count() != 1 {
                return Err(ActiveReblitPackageCmdlineInputsError::InvalidCoordinate { binding_index });
            }
            Ok(PackageCmdlineScope::Global)
        }
        BootAssetRole::KernelCmdline { version } => {
            let relative = logical
                .strip_prefix(Path::new("/usr/lib/kernel"))
                .map_err(|_| ActiveReblitPackageCmdlineInputsError::InvalidCoordinate { binding_index })?;
            let mut components = relative.components();
            let exact_version = components.next().map(|component| component.as_os_str()) == Some(OsStr::new(version));
            if !exact_version || components.next().is_none() || components.next().is_some() {
                return Err(ActiveReblitPackageCmdlineInputsError::InvalidCoordinate { binding_index });
            }
            Ok(PackageCmdlineScope::Kernel {
                version: clone_string(version, "package command-line kernel version")?.into_boxed_str(),
            })
        }
        _ => Err(ActiveReblitPackageCmdlineInputsError::InvalidCoordinate { binding_index }),
    }
}

fn coordinate_matches(expected: &PreparedActiveReblitPackageCmdline, asset: &BoundActiveReblitBootAsset<'_>) -> bool {
    if asset.state_id() != expected.state_id
        || asset.logical_path() != expected.logical_path
        || asset.logical_path().file_name() != Some(expected.filename.as_os_str())
        || asset.digest() != expected.digest
        || asset.length() != expected.length
    {
        return false;
    }
    match (&expected.scope, asset.role()) {
        (PackageCmdlineScope::Global, BootAssetRole::GlobalCmdline) => true,
        (
            PackageCmdlineScope::Kernel {
                version: expected_version,
            },
            BootAssetRole::KernelCmdline {
                version: actual_version,
            },
        ) => expected_version.as_ref() == actual_version,
        _ => false,
    }
}

pub(super) fn read_exact_source_at(
    descriptor: BorrowedFd<'_>,
    length: usize,
    binding_index: usize,
    budget: &mut PackageCmdlineBudget,
) -> Result<Vec<u8>, ActiveReblitPackageCmdlineInputsError> {
    read_exact_source_at_with(
        descriptor,
        length,
        binding_index,
        budget,
        |descriptor, bytes, offset| {
            // SAFETY: the borrowed descriptor remains live, `bytes` is a writable
            // slice, and the explicit offset has already been bounded to `off_t`.
            let result =
                unsafe { nix::libc::pread(descriptor.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), offset) };
            if result >= 0 {
                Ok(usize::try_from(result).expect("nonnegative pread result fits usize"))
            } else {
                Err(io::Error::last_os_error())
            }
        },
    )
}

pub(super) fn read_exact_source_at_with<F>(
    descriptor: BorrowedFd<'_>,
    length: usize,
    binding_index: usize,
    budget: &mut PackageCmdlineBudget,
    mut read_at: F,
) -> Result<Vec<u8>, ActiveReblitPackageCmdlineInputsError>
where
    F: FnMut(BorrowedFd<'_>, &mut [u8], i64) -> io::Result<usize>,
{
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|source| ActiveReblitPackageCmdlineInputsError::Allocation {
            resource: "sealed package command-line bytes",
            source,
        })?;
    bytes.resize(length, 0);

    let mut offset = 0usize;
    let mut interruptions = 0usize;
    while offset < length {
        budget.step("explicit-offset sealed source read")?;
        let file_offset = i64::try_from(offset).expect("package command-line source bound fits off_t");
        match read_at(descriptor, &mut bytes[offset..], file_offset) {
            Ok(read) => {
                if read == 0 {
                    return Err(read_error(
                        binding_index,
                        io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "sealed command-line source shortened during read",
                        ),
                    ));
                }
                if read > length - offset {
                    return Err(read_error(
                        binding_index,
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "pread exceeded the requested command-line range",
                        ),
                    ));
                }
                offset += read;
                interruptions = 0;
            }
            Err(source) if source.kind() != io::ErrorKind::Interrupted => {
                return Err(read_error(binding_index, source));
            }
            Err(_) => {
                if interruptions >= MAX_PACKAGE_CMDLINE_INTERRUPTED_RETRIES {
                    return Err(read_error(
                        binding_index,
                        io::Error::new(
                            io::ErrorKind::Interrupted,
                            "sealed command-line read exceeded interruption limit",
                        ),
                    ));
                }
                interruptions += 1;
            }
        }
    }
    budget.require_deadline("sealed source post-read checkpoint")?;
    Ok(bytes)
}

pub(super) fn require_digest(
    binding_index: usize,
    expected: u128,
    bytes: &[u8],
) -> Result<(), ActiveReblitPackageCmdlineInputsError> {
    let actual = xxhash_rust::xxh3::xxh3_128(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(ActiveReblitPackageCmdlineInputsError::DigestMismatch {
            binding_index,
            expected,
            actual,
        })
    }
}

fn read_error(binding_index: usize, source: io::Error) -> ActiveReblitPackageCmdlineInputsError {
    ActiveReblitPackageCmdlineInputsError::ReadSource { binding_index, source }
}

fn copy_state_ids(states: &[state::Id]) -> Result<Box<[state::Id]>, ActiveReblitPackageCmdlineInputsError> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(states.len())
        .map_err(|source| ActiveReblitPackageCmdlineInputsError::Allocation {
            resource: "package command-line state projection",
            source,
        })?;
    copied.extend_from_slice(states);
    Ok(copied.into_boxed_slice())
}

fn clone_path(path: &Path, resource: &'static str) -> Result<PathBuf, ActiveReblitPackageCmdlineInputsError> {
    Ok(PathBuf::from(clone_os_string(path.as_os_str(), resource)?))
}

fn clone_os_string(value: &OsStr, resource: &'static str) -> Result<OsString, ActiveReblitPackageCmdlineInputsError> {
    let mut cloned = OsString::new();
    cloned
        .try_reserve_exact(value.as_bytes().len())
        .map_err(|source| ActiveReblitPackageCmdlineInputsError::Allocation { resource, source })?;
    cloned.push(value);
    Ok(cloned)
}

fn clone_string(value: &str, resource: &'static str) -> Result<String, ActiveReblitPackageCmdlineInputsError> {
    let mut cloned = String::new();
    cloned
        .try_reserve_exact(value.len())
        .map_err(|source| ActiveReblitPackageCmdlineInputsError::Allocation { resource, source })?;
    cloned.push_str(value);
    Ok(cloned)
}

pub(super) fn validated_state_position(
    states: &[state::Id],
    state_id: state::Id,
    binding_index: usize,
    budget: &mut PackageCmdlineBudget,
) -> Result<u16, ActiveReblitPackageCmdlineInputsError> {
    for (position, candidate) in states.iter().enumerate() {
        budget.step("projected state position lookup")?;
        if *candidate != state_id {
            continue;
        }
        return u16::try_from(position).map_err(|_| ActiveReblitPackageCmdlineInputsError::StatePositionLimit {
            binding_index,
            limit: u16::MAX as usize,
            actual: position,
        });
    }
    Err(ActiveReblitPackageCmdlineInputsError::AssetStateOutsideProjection {
        binding_index,
        state: i32::from(state_id),
    })
}
