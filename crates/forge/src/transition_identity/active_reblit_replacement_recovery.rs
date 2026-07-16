//! Phase-authorized recovery of a replacement wrapper stranded before chmod.

use std::{
    error::Error as StdError,
    ffi::{CStr, CString},
    io,
    os::{
        fd::AsRawFd as _,
        unix::{
            ffi::OsStringExt as _,
            fs::{MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::{
    Installation,
    client::ActiveReblitReplacementMutationAuthorityProvider,
    installation,
    linux_fs::{chmod_path_descriptor, controlled_resolution, openat2_file},
    transition_journal::{ForwardPhase, Operation, Phase, StorageError, TransitionJournalStore, TransitionRecord},
};

use super::{PRIVATE_DIRECTORY_MODE, QUARANTINE_RELATIVE, RetainedDirectory};

const REPLACEMENT_PREFIX: &str = "replaced-active-reblit-wrapper-";
const MAX_REPLACEMENT_INDICES: usize = 256;
const MAX_QUARANTINE_ENTRIES: usize = 1_024;

#[cfg(test)]
std::thread_local! {
    static BEFORE_NORMALIZATION_PREFLIGHT: std::cell::RefCell<Option<Box<dyn FnOnce(&TransitionJournalStore)>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_before_active_reblit_replacement_normalization_preflight(
    hook: impl FnOnce(&TransitionJournalStore) + 'static,
) {
    BEFORE_NORMALIZATION_PREFLIGHT.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn before_normalization_preflight(journal: &TransitionJournalStore) {
    BEFORE_NORMALIZATION_PREFLIGHT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(journal);
        }
    });
}

#[cfg(not(test))]
fn before_normalization_preflight(_journal: &TransitionJournalStore) {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActiveReblitReplacementRecovery {
    NotApplicable,
    Absent,
    AlreadyCanonical,
    Normalized,
}

/// Complete a mkdir-before-chmod crash prefix under the exact durable phase.
///
/// This is deliberately outside startup's diagnostic inventory. The caller
/// retains the installation-wide writer lock and exclusive journal store. A
/// restrictive inode is changed monotonically to 0700 and never restored.
pub(crate) fn recover_active_reblit_replacement_residue(
    authority_provider: &mut ActiveReblitReplacementMutationAuthorityProvider<'_>,
) -> Result<ActiveReblitReplacementRecovery, ActiveReblitReplacementRecoveryError> {
    let (installation, journal, expected) = authority_provider.recovery_context();
    let mut authority = None;
    recover_active_reblit_replacement_residue_with(installation, journal, expected, || {
        if authority.is_none() {
            authority = Some(authority_provider.prepare()?);
        }
        authority
            .as_ref()
            .expect("startup replacement mutation authority was prepared")
            .revalidate()
    })
}

#[cfg(test)]
pub(crate) fn recover_active_reblit_replacement_residue_with_explicit_context_for_test(
    installation: &Installation,
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
    authority_provider: &mut ActiveReblitReplacementMutationAuthorityProvider<'_>,
) -> Result<ActiveReblitReplacementRecovery, ActiveReblitReplacementRecoveryError> {
    authority_provider
        .require_exact_context(installation, journal, expected)
        .map_err(|source| ActiveReblitReplacementRecoveryError::MutationAuthority {
            source: Box::new(source),
        })?;
    recover_active_reblit_replacement_residue(authority_provider)
}

#[cfg(test)]
pub(crate) fn recover_active_reblit_replacement_residue_for_namespace_test(
    installation: &Installation,
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<ActiveReblitReplacementRecovery, ActiveReblitReplacementRecoveryError> {
    recover_active_reblit_replacement_residue_with(installation, journal, expected, || {
        Ok::<(), std::convert::Infallible>(())
    })
}

fn recover_active_reblit_replacement_residue_with<E>(
    installation: &Installation,
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
    mut revalidate_mutation_authority: impl FnMut() -> Result<(), E>,
) -> Result<ActiveReblitReplacementRecovery, ActiveReblitReplacementRecoveryError>
where
    E: StdError + Send + Sync + 'static,
{
    if !phase_authorizes_recovery(expected) {
        return Ok(ActiveReblitReplacementRecovery::NotApplicable);
    }

    require_exact_journal(journal, expected)?;
    installation.revalidate_mutable_namespace()?;
    let quarantine_path = installation.state_quarantine_dir();
    let quarantine = RetainedDirectory::open_beneath(
        installation.root_directory(),
        QUARANTINE_RELATIVE,
        quarantine_path.clone(),
    )?;
    quarantine.require_retained()?;

    let state = expected
        .previous
        .id
        .expect("validated active-reblit journal has previous state ID");
    let prefix = format!("{REPLACEMENT_PREFIX}{state}-{}-", expected.previous.tree_token.as_str());
    let matches = matching_replacements(&quarantine, prefix.as_bytes())?;
    let name = match matches.as_slice() {
        [] => return Ok(ActiveReblitReplacementRecovery::Absent),
        [name] => name,
        _ => {
            return Err(ActiveReblitReplacementRecoveryError::Ambiguous {
                path: quarantine_path.clone(),
                count: matches.len(),
            });
        }
    };
    let encoded = CString::new(name.as_slice()).expect("validated replacement name contains no NUL");
    let path = quarantine.path.join(std::ffi::OsString::from_vec(name.clone()));
    let pinned = pin_replacement(&quarantine, &encoded, &path)?;
    let before = ResidueWitness::read(&pinned, &path)?;
    before.require_recoverable(&path)?;

    // Keep every fallible inventory and namespace reopen before the final
    // semantic observation. No pathname-based operation follows this proof.
    before_normalization_preflight(journal);
    require_exact_journal(journal, expected)?;
    installation.revalidate_mutable_namespace()?;
    quarantine.require_retained()?;
    quarantine.revalidate_beneath(installation.root_directory(), QUARANTINE_RELATIVE)?;
    require_exact_singleton(&quarantine, prefix.as_bytes(), name, &path)?;
    require_named_replacement(&quarantine, &encoded, before, &path)?;

    // A canonical wrapper may already contain the preserved candidate during
    // rollback from CandidatePrepared. Its ordinary phase policy owns that
    // interpretation; this narrow mkdir-residue repair must not require it to
    // remain empty or otherwise mutate it.
    if before.mode == PRIVATE_DIRECTORY_MODE {
        return Ok(ActiveReblitReplacementRecovery::AlreadyCanonical);
    }

    revalidate_mutation_authority().map_err(|source| ActiveReblitReplacementRecoveryError::MutationAuthority {
        source: Box::new(source),
    })?;
    require_exact_journal(journal, expected)?;
    installation.revalidate_mutable_namespace()?;
    quarantine.require_retained()?;
    quarantine.revalidate_beneath(installation.root_directory(), QUARANTINE_RELATIVE)?;
    require_exact_singleton(&quarantine, prefix.as_bytes(), name, &path)?;
    require_named_replacement(&quarantine, &encoded, before, &path)?;
    chmod_path_descriptor(&pinned, PRIVATE_DIRECTORY_MODE).map_err(|source| {
        ActiveReblitReplacementRecoveryError::Io {
            operation: "normalize retained active-reblit replacement residue",
            path: path.clone(),
            source,
        }
    })?;
    revalidate_mutation_authority().map_err(|source| ActiveReblitReplacementRecoveryError::MutationAuthority {
        source: Box::new(source),
    })?;

    // Open and sync the exact normalized inode before stricter post-mutation
    // checks. If an ACL or payload makes admission fail, the safe 0700 mode is
    // intentionally left in place and its metadata is still made durable.
    let readable = open_normalized_replacement(&quarantine, &encoded, before, &path)?;
    readable
        .sync_all()
        .map_err(|source| ActiveReblitReplacementRecoveryError::Io {
            operation: "sync normalized active-reblit replacement",
            path: path.clone(),
            source,
        })?;
    quarantine
        .file
        .sync_all()
        .map_err(|source| ActiveReblitReplacementRecoveryError::Io {
            operation: "sync active-reblit replacement parent",
            path: quarantine.path.clone(),
            source,
        })?;

    let replacement = quarantine.open_child(&encoded, path.clone())?;
    replacement.require_retained()?;
    replacement.require_exact_entries(&[])?;
    replacement.sync("sync exact empty active-reblit replacement")?;
    quarantine.sync("sync active-reblit replacement parent after validation")?;

    require_exact_journal(journal, expected)?;
    installation.revalidate_mutable_namespace()?;
    quarantine.require_retained()?;
    quarantine.revalidate_beneath(installation.root_directory(), QUARANTINE_RELATIVE)?;
    replacement.revalidate_child(&quarantine, &encoded)?;
    replacement.require_retained()?;
    replacement.require_exact_entries(&[])?;
    require_exact_singleton(&quarantine, prefix.as_bytes(), name, &path)?;
    revalidate_mutation_authority().map_err(|source| ActiveReblitReplacementRecoveryError::MutationAuthority {
        source: Box::new(source),
    })?;
    require_exact_journal(journal, expected)?;

    Ok(ActiveReblitReplacementRecovery::Normalized)
}

fn matching_replacements(
    quarantine: &RetainedDirectory,
    prefix: &[u8],
) -> Result<Vec<Vec<u8>>, ActiveReblitReplacementRecoveryError> {
    Ok(quarantine
        .entries(MAX_QUARANTINE_ENTRIES)?
        .into_iter()
        .filter(|name| replacement_index(name, prefix).is_some())
        .collect())
}

fn require_exact_singleton(
    quarantine: &RetainedDirectory,
    prefix: &[u8],
    expected_name: &[u8],
    path: &Path,
) -> Result<(), ActiveReblitReplacementRecoveryError> {
    let matches = matching_replacements(quarantine, prefix)?;
    match matches.as_slice() {
        [actual] if actual.as_slice() == expected_name => Ok(()),
        [.., _] if matches.len() > 1 => Err(ActiveReblitReplacementRecoveryError::Ambiguous {
            path: quarantine.path.clone(),
            count: matches.len(),
        }),
        _ => Err(ActiveReblitReplacementRecoveryError::Changed { path: path.to_owned() }),
    }
}

fn phase_authorizes_recovery(record: &TransitionRecord) -> bool {
    if record.operation != Operation::ActiveReblit {
        return false;
    }
    record
        .rollback
        .as_ref()
        .map_or(record.phase == Phase::CandidatePrepared, |rollback| {
            rollback.source == ForwardPhase::CandidatePrepared
        })
}

fn replacement_index(name: &[u8], prefix: &[u8]) -> Option<usize> {
    let index = name.strip_prefix(prefix)?;
    if index.is_empty() || (index.len() > 1 && index[0] == b'0') || !index.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let index = std::str::from_utf8(index).ok()?.parse().ok()?;
    (index < MAX_REPLACEMENT_INDICES).then_some(index)
}

fn pin_replacement(
    quarantine: &RetainedDirectory,
    name: &CStr,
    path: &Path,
) -> Result<std::fs::File, ActiveReblitReplacementRecoveryError> {
    openat2_file(
        quarantine.file.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(|source| ActiveReblitReplacementRecoveryError::Io {
        operation: "pin active-reblit replacement residue",
        path: path.to_owned(),
        source,
    })
}

fn require_named_replacement(
    quarantine: &RetainedDirectory,
    name: &CStr,
    expected: ResidueWitness,
    path: &Path,
) -> Result<(), ActiveReblitReplacementRecoveryError> {
    let named = pin_replacement(quarantine, name, path)?;
    let actual = ResidueWitness::read(&named, path)?;
    if actual == expected {
        Ok(())
    } else {
        Err(ActiveReblitReplacementRecoveryError::Changed { path: path.to_owned() })
    }
}

fn open_normalized_replacement(
    quarantine: &RetainedDirectory,
    name: &CStr,
    expected: ResidueWitness,
    path: &Path,
) -> Result<std::fs::File, ActiveReblitReplacementRecoveryError> {
    let file = openat2_file(
        quarantine.file.as_raw_fd(),
        name,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
    )
    .map_err(|source| ActiveReblitReplacementRecoveryError::Io {
        operation: "open normalized active-reblit replacement",
        path: path.to_owned(),
        source,
    })?;
    let actual = ResidueWitness::read(&file, path)?;
    if actual.device != expected.device
        || actual.inode != expected.inode
        || actual.owner != expected.owner
        || actual.mode != PRIVATE_DIRECTORY_MODE
    {
        return Err(ActiveReblitReplacementRecoveryError::Changed { path: path.to_owned() });
    }
    Ok(file)
}

fn require_exact_journal(
    journal: &TransitionJournalStore,
    expected: &TransitionRecord,
) -> Result<(), ActiveReblitReplacementRecoveryError> {
    match journal.load()? {
        Some(actual) if actual == *expected => Ok(()),
        actual => Err(ActiveReblitReplacementRecoveryError::JournalChanged {
            expected_generation: expected.generation,
            actual_generation: actual.map(|record| record.generation),
        }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResidueWitness {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    kind: u32,
}

impl ResidueWitness {
    fn read(file: &std::fs::File, path: &Path) -> Result<Self, ActiveReblitReplacementRecoveryError> {
        let metadata = file
            .metadata()
            .map_err(|source| ActiveReblitReplacementRecoveryError::Io {
                operation: "inspect active-reblit replacement residue",
                path: path.to_owned(),
                source,
            })?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            mode: metadata.permissions().mode() & 0o7777,
            kind: metadata.mode() & nix::libc::S_IFMT,
        })
    }

    fn require_recoverable(self, path: &Path) -> Result<(), ActiveReblitReplacementRecoveryError> {
        // The mode is an exact subset of the creator's 0700 request. In
        // particular, the group-class ACL mask and all other/special bits are
        // zero before the monotonic descriptor-bound chmod.
        if self.kind == nix::libc::S_IFDIR
            && self.owner == unsafe { nix::libc::geteuid() }
            && self.mode & !PRIVATE_DIRECTORY_MODE == 0
        {
            Ok(())
        } else {
            Err(ActiveReblitReplacementRecoveryError::UnsafeResidue {
                path: path.to_owned(),
                owner: self.owner,
                mode: self.mode,
            })
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ActiveReblitReplacementRecoveryError {
    #[error("revalidate installation around active-reblit replacement recovery")]
    Installation(#[from] installation::Error),
    #[error("inspect exact journal around active-reblit replacement recovery")]
    Journal(#[from] StorageError),
    #[error("authenticate active-reblit replacement namespace")]
    Namespace(#[from] super::Error),
    #[error("active-reblit replacement journal changed from generation {expected_generation} to {actual_generation:?}")]
    JournalChanged {
        expected_generation: u64,
        actual_generation: Option<u64>,
    },
    #[error("multiple ({count}) current-transition active-reblit replacements exist below `{}`", path.display())]
    Ambiguous { path: PathBuf, count: usize },
    #[error("unsafe active-reblit replacement residue `{}` (uid={owner}, mode={mode:04o})", path.display())]
    UnsafeResidue { path: PathBuf, owner: u32, mode: u32 },
    #[error("active-reblit replacement changed at `{}`", path.display())]
    Changed { path: PathBuf },
    #[error("revalidate database and active-state authority around active-reblit replacement normalization")]
    MutationAuthority {
        #[source]
        source: Box<dyn StdError + Send + Sync + 'static>,
    },
    #[error("{operation} at `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
