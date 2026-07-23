//! Read-only proof construction for metadata already present in an archived candidate.
//!
//! Unlike publication, this typestate never creates, replaces, normalizes, or
//! syncs an inode. The expected bytes come from the caller's independent
//! declarative inputs before either canonical output is opened for proof.

use super::*;

#[cfg(test)]
std::thread_local! {
    static AFTER_EXISTING_RELEASE_RETAINED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[allow(dead_code)] // Narrow race hook; production has no callback surface.
pub(crate) fn arm_after_existing_release_retained(hook: impl FnOnce() + 'static) {
    AFTER_EXISTING_RELEASE_RETAINED.with(|slot| {
        assert!(
            slot.borrow_mut().replace(Box::new(hook)).is_none(),
            "existing metadata verification hook is already armed"
        );
    });
}

fn after_existing_release_retained() {
    #[cfg(test)]
    AFTER_EXISTING_RELEASE_RETAINED.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

/// Exact archived candidate whose existing canonical metadata has not yet
/// been proved against independently supplied expectations.
#[derive(Debug)]
pub(crate) struct CandidateMetadataVerification {
    usr: File,
    usr_path: PathBuf,
    lib: RetainedDirectory,
}

impl CandidateMetadataVerification {
    /// Retain the exact candidate and policy input namespace before either
    /// independent expected output is derived, then open an already-existing
    /// `usr/lib` without repair.
    pub(crate) fn begin(usr: &File, usr_path: &Path) -> Result<Self, CandidateMetadataError> {
        let usr = clone_candidate_usr(usr, usr_path)?;
        let lib = RetainedDirectory::open(&usr, LIB_NAME, usr_path.join("lib"))?;
        lib.require_named(&usr, LIB_NAME)?;
        Ok(Self {
            usr,
            usr_path: usr_path.to_owned(),
            lib,
        })
    }

    /// Read only the optional policy input through the exact retained `lib`.
    /// Neither canonical output is consulted while expectations are derived.
    pub(crate) fn read_optional_os_info(&self) -> Result<Option<Vec<u8>>, CandidateMetadataError> {
        read_optional_input(&self.lib, OS_INFO_NAME, &self.lib.path.join("os-info.json"))
    }

    /// Prove both existing canonical names against independent expected bytes
    /// and return the same descriptor-owning proof used after publication.
    pub(crate) fn prove(
        self,
        outputs: CandidateMetadataOutputs,
    ) -> Result<CandidateMetadataProof, CandidateMetadataError> {
        let CandidateMetadataOutputs {
            os_release: release_bytes,
            system_model: snapshot_output,
        } = outputs;
        let Self { usr, usr_path, lib } = self;
        snapshot_output.revalidate_authority()?;

        lib.require_named(&usr, LIB_NAME)?;
        require_alternate_declarations_absent(&lib, &snapshot_output)?;
        let release = retain_existing_published(&lib, OS_RELEASE_NAME, &release_bytes, &lib.path.join("os-release"))?;
        after_existing_release_retained();
        lib.require_named(&usr, LIB_NAME)?;
        require_alternate_declarations_absent(&lib, &snapshot_output)?;
        let snapshot = retain_existing_published(
            &lib,
            snapshot_output.file_name(),
            snapshot_output.bytes(),
            &snapshot_output.path_in(&lib.path),
        )?;

        let proof = CandidateMetadataProof {
            usr,
            usr_path,
            lib,
            release,
            release_bytes,
            snapshot,
            snapshot_output,
        };
        proof.revalidate()?;
        Ok(proof)
    }
}

fn retain_existing_published(
    directory: &RetainedDirectory,
    name: &CStr,
    expected: &[u8],
    path: &Path,
) -> Result<PreparedFile, CandidateMetadataError> {
    directory.require_retained()?;
    let pinned = openat2_file(
        directory.file.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(|source| metadata_io("retain existing candidate metadata", path, source))?;
    let witness = published_witness(&pinned, path, expected.len())?;
    let file = open_path_descriptor_readonly_until(
        &pinned,
        Instant::now()
            .checked_add(DESCRIPTOR_READ_TIMEOUT)
            .unwrap_or_else(Instant::now),
    )
    .map_err(|source| metadata_io("open existing candidate metadata for proof", path, source))?;
    if published_witness(&file, path, expected.len())? != witness {
        return Err(CandidateMetadataError::FileChanged { path: path.to_owned() });
    }
    let retained = PreparedFile {
        file,
        identity: (witness.device, witness.inode),
    };
    require_published(directory, name, &retained.file, retained.identity, expected, path)?;
    directory.require_retained()?;
    Ok(retained)
}
