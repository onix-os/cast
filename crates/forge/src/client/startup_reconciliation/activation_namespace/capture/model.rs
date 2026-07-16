use super::*;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct InodeWitness {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) mode: u32,
    pub(super) owner: u32,
    pub(super) group: u32,
    pub(super) links: u64,
    pub(super) length: u64,
    pub(super) modified_seconds: i64,
    pub(super) modified_nanoseconds: i64,
    pub(super) changed_seconds: i64,
    pub(super) changed_nanoseconds: i64,
}

impl InodeWitness {
    pub(super) fn read(file: &File, path: &Path) -> Result<Self, CaptureError> {
        let metadata = file.metadata().map_err(|source| CaptureError::Io {
            operation: "inspect retained activation-namespace inode",
            path: path.to_owned(),
            source,
        })?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }

    pub(super) fn kind(self) -> u32 {
        self.mode & nix::libc::S_IFMT
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum TreeLocation {
    Live,
    Staging,
    State(i32),
    ArchivedCandidateParking { state: i32, token: String, index: usize },
    PreviousParking { state: i32, index: usize },
    TransitionQuarantine,
    ActiveReblitWrapper { state: i32, index: usize },
    AmbientQuarantine(Vec<u8>),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum RootEntryRole {
    Staging,
    Isolation,
    State(i32),
    ArchivedCandidateParking { state: i32, token: String, index: usize },
    PreviousParking { state: i32, index: usize },
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum QuarantineEntryRole {
    Transition,
    ActiveReblitWrapper { state: i32, index: usize },
    Ambient,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StateIdObservation {
    Absent,
    Canonical(i32),
    Corrupt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StateIdCorruption {
    TemporaryName,
    UnsafeMetadata,
    InvalidContent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum StateIdFingerprint {
    Absent,
    Canonical {
        state: i32,
        witness: InodeWitness,
        bytes: Vec<u8>,
    },
    Corrupt {
        reason: StateIdCorruption,
        state: Option<InodeWitness>,
        temporary: Option<InodeWitness>,
        bytes: Option<Vec<u8>>,
    },
}

impl StateIdFingerprint {
    pub(super) fn canonical_state(&self) -> Option<i32> {
        match self {
            Self::Canonical { state, .. } => Some(*state),
            Self::Absent | Self::Corrupt { .. } => None,
        }
    }

    pub(crate) fn observation(&self) -> StateIdObservation {
        match self {
            Self::Absent => StateIdObservation::Absent,
            Self::Canonical { state, .. } => StateIdObservation::Canonical(*state),
            Self::Corrupt { .. } => StateIdObservation::Corrupt,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct RetainedStateId {
    pub(super) file: Option<File>,
    pub(super) temporary: Option<File>,
    pub(super) fingerprint: StateIdFingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SlotFingerprint {
    pub(super) state: i32,
    pub(super) token: String,
    pub(super) witness: InodeWitness,
}

#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct RetainedSlotLink {
    pub(super) file: File,
    pub(super) name: Vec<u8>,
    pub(super) path: PathBuf,
    pub(super) fingerprint: SlotFingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsrFingerprint {
    pub(crate) location: TreeLocation,
    pub(crate) token: String,
    pub(super) directory: InodeWitness,
    pub(super) marker: InodeWitness,
    pub(super) state_id: StateIdFingerprint,
    pub(crate) runtime: RuntimeTreeIdentity,
}

impl UsrFingerprint {
    pub(crate) fn state_id_observation(&self) -> StateIdObservation {
        self.state_id.observation()
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct RetainedUsr {
    pub(super) store: TreeMarkerStore,
    pub(super) marker: RetainedTreeMarker,
    pub(super) state_id: RetainedStateId,
    pub(super) fingerprint: UsrFingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WrapperFingerprint {
    pub(crate) name: Vec<u8>,
    pub(super) witness: InodeWitness,
    pub(crate) role: TreeLocation,
    pub(super) entries: Vec<(Vec<u8>, InodeWitness)>,
    pub(crate) usr: Option<UsrFingerprint>,
    pub(super) slot: Option<SlotFingerprint>,
}

impl WrapperFingerprint {
    pub(in crate::client::startup_reconciliation::activation_namespace) fn slot_identity(&self) -> Option<(i32, &str)> {
        self.slot.as_ref().map(|slot| (slot.state, slot.token.as_str()))
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct RetainedWrapper {
    pub(super) directory: File,
    pub(super) fingerprint: WrapperFingerprint,
    pub(super) usr: Option<RetainedUsr>,
    pub(super) slot: Option<RetainedSlotLink>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RootAbiLinkFingerprint {
    pub(super) name: Vec<u8>,
    pub(super) target: Vec<u8>,
    pub(super) witness: InodeWitness,
}

#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct RetainedRootAbiLink {
    pub(super) file: File,
    pub(super) fingerprint: RootAbiLinkFingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RootAbiFingerprint {
    pub(super) links: Vec<Option<RootAbiLinkFingerprint>>,
}

impl RootAbiFingerprint {
    pub(crate) fn is_complete(&self) -> bool {
        self.links.iter().all(Option::is_some)
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct RetainedRootAbi {
    pub(super) links: Vec<Option<RetainedRootAbiLink>>,
    pub(super) fingerprint: RootAbiFingerprint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NamespaceFingerprint {
    pub(super) root: InodeWitness,
    pub(super) roots: InodeWitness,
    pub(super) quarantine: InodeWitness,
    pub(super) epoch: RuntimeEpoch,
    pub(super) live: UsrFingerprint,
    pub(super) root_abi: RootAbiFingerprint,
    pub(super) isolation_abi: RootAbiFingerprint,
    pub(super) roots_entries: Vec<WrapperFingerprint>,
    pub(super) quarantine_entries: Vec<WrapperFingerprint>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct NamespaceSnapshot {
    pub(super) root: File,
    pub(super) root_path: PathBuf,
    pub(super) roots: File,
    pub(super) roots_path: PathBuf,
    pub(super) quarantine: File,
    pub(super) quarantine_path: PathBuf,
    pub(super) live: RetainedUsr,
    pub(super) root_abi: RetainedRootAbi,
    pub(super) isolation_abi: RetainedRootAbi,
    pub(super) roots_entries: Vec<RetainedWrapper>,
    pub(super) quarantine_entries: Vec<RetainedWrapper>,
    pub(super) fingerprint: NamespaceFingerprint,
}

impl NamespaceSnapshot {
    pub(crate) fn fingerprint(&self) -> &NamespaceFingerprint {
        &self.fingerprint
    }

    pub(crate) fn epoch(&self) -> &RuntimeEpoch {
        &self.fingerprint.epoch
    }

    pub(crate) fn root_abi(&self) -> &RootAbiFingerprint {
        &self.fingerprint.root_abi
    }

    pub(crate) fn isolation_abi(&self) -> &RootAbiFingerprint {
        &self.fingerprint.isolation_abi
    }

    pub(crate) fn wrappers(&self) -> impl Iterator<Item = &WrapperFingerprint> {
        self.fingerprint
            .roots_entries
            .iter()
            .chain(&self.fingerprint.quarantine_entries)
    }

    pub(crate) fn trees(&self) -> impl Iterator<Item = &UsrFingerprint> {
        std::iter::once(&self.fingerprint.live).chain(
            self.fingerprint
                .roots_entries
                .iter()
                .chain(&self.fingerprint.quarantine_entries)
                .filter_map(|wrapper| wrapper.usr.as_ref()),
        )
    }

    pub(crate) fn revalidate_retained(&self) -> Result<(), CaptureError> {
        let mut budget = Budget::new()?;
        require_witness(
            controlled_directory_witness(&self.root, &self.root_path)?,
            self.fingerprint.root,
            &self.root_path,
        )?;
        require_witness(
            controlled_directory_witness(&self.roots, &self.roots_path)?,
            self.fingerprint.roots,
            &self.roots_path,
        )?;
        require_witness(
            controlled_directory_witness(&self.quarantine, &self.quarantine_path)?,
            self.fingerprint.quarantine,
            &self.quarantine_path,
        )?;

        // Bind both retained namespace roots back to their complete public
        // names.  Installation revalidation authenticates `.cast`, but not
        // these children, and descriptor metadata alone would miss a
        // post-capture substitution of `.cast/root` or `.cast/quarantine`.
        revalidate_named_entry(
            &self.root,
            b".cast/root",
            self.fingerprint.roots,
            &self.roots_path,
            &mut budget,
        )?;
        revalidate_named_entry(
            &self.root,
            b".cast/quarantine",
            self.fingerprint.quarantine,
            &self.quarantine_path,
            &mut budget,
        )?;

        revalidate_named_entry(
            &self.root,
            b"usr",
            self.live.fingerprint.directory,
            &self.root_path.join("usr"),
            &mut budget,
        )?;
        revalidate_usr(&self.live, &mut budget)?;
        revalidate_root_abi(&self.root_abi, &self.root, &self.root_path, &mut budget)?;
        revalidate_wrapper_set(&self.roots, &self.roots_path, &self.roots_entries, &mut budget)?;
        revalidate_wrapper_set(
            &self.quarantine,
            &self.quarantine_path,
            &self.quarantine_entries,
            &mut budget,
        )?;
        let isolation = self
            .roots_entries
            .iter()
            .find(|wrapper| wrapper.fingerprint.name == b"isolation")
            .ok_or(CaptureError::FixedWrapperMissing { name: "isolation" })?;
        revalidate_root_abi(
            &self.isolation_abi,
            &isolation.directory,
            &self.roots_path.join("isolation"),
            &mut budget,
        )?;
        Ok(())
    }
}

fn require_witness(actual: InodeWitness, expected: InodeWitness, path: &Path) -> Result<(), CaptureError> {
    if actual == expected {
        Ok(())
    } else {
        Err(CaptureError::InodeChanged { path: path.to_owned() })
    }
}

fn revalidate_wrapper_set(
    parent: &File,
    parent_path: &Path,
    wrappers: &[RetainedWrapper],
    budget: &mut Budget,
) -> Result<(), CaptureError> {
    let actual_names = directory_names(parent, parent_path, MAX_NAMESPACE_ENTRIES, budget)?;
    let expected_names = wrappers
        .iter()
        .map(|wrapper| wrapper.fingerprint.name.clone())
        .collect::<Vec<_>>();
    if actual_names != expected_names {
        return Err(CaptureError::DirectoryContentsChanged {
            path: parent_path.to_owned(),
        });
    }
    for wrapper in wrappers {
        let path = parent_path.join(os(&wrapper.fingerprint.name));
        revalidate_named_entry(
            parent,
            &wrapper.fingerprint.name,
            wrapper.fingerprint.witness,
            &path,
            budget,
        )?;
        require_witness(
            controlled_directory_witness(&wrapper.directory, &path)?,
            wrapper.fingerprint.witness,
            &path,
        )?;
        let actual_entries = directory_names(&wrapper.directory, &path, MAX_WRAPPER_ENTRIES, budget)?;
        let expected_entries = wrapper
            .fingerprint
            .entries
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        if actual_entries != expected_entries {
            return Err(CaptureError::DirectoryContentsChanged { path });
        }
        for (name, witness) in &wrapper.fingerprint.entries {
            revalidate_named_entry(&wrapper.directory, name, *witness, &path.join(os(name)), budget)?;
        }
        if let Some(usr) = &wrapper.usr {
            revalidate_usr(usr, budget)?;
        }
        if let Some(slot) = &wrapper.slot {
            require_witness(
                InodeWitness::read(&slot.file, &slot.path)?,
                slot.fingerprint.witness,
                &slot.path,
            )?;
        }
    }
    Ok(())
}

fn revalidate_named_entry(
    parent: &File,
    name: &[u8],
    expected: InodeWitness,
    path: &Path,
    budget: &mut Budget,
) -> Result<(), CaptureError> {
    let file = open_optional_path(parent, cstring(name)?.as_c_str(), path, budget)?
        .ok_or_else(|| CaptureError::InodeChanged { path: path.to_owned() })?;
    require_witness(InodeWitness::read(&file, path)?, expected, path)
}

fn revalidate_usr(usr: &RetainedUsr, budget: &mut Budget) -> Result<(), CaptureError> {
    let path = usr.store.display_path();
    require_witness(
        safe_usr_witness(usr.store.retained_directory(), path)?,
        usr.fingerprint.directory,
        path,
    )?;
    let named_marker = usr
        .marker
        .read_named_for_transition(&usr.store)
        .map_err(CaptureError::TreeMarker)?;
    usr.marker
        .require_same_marker(&named_marker)
        .map_err(CaptureError::TreeMarker)?;
    named_marker.revalidate(&usr.store).map_err(CaptureError::TreeMarker)?;
    revalidate_state_id(&usr.state_id, usr.store.retained_directory(), path, budget)?;
    let runtime = RuntimeTreeIdentity::capture_directory(usr.store.retained_directory()).map_err(|source| {
        CaptureError::RuntimeTree {
            path: path.to_owned(),
            source,
        }
    })?;
    if runtime != usr.fingerprint.runtime {
        return Err(CaptureError::InodeChanged { path: path.to_owned() });
    }
    Ok(())
}

fn revalidate_state_id(
    retained: &RetainedStateId,
    usr: &File,
    usr_path: &Path,
    budget: &mut Budget,
) -> Result<(), CaptureError> {
    let state_path = usr_path.join(".stateID");
    let temporary_path = usr_path.join(".cast-state-id.tmp");
    let (expected_state, expected_temporary, expected_bytes) = match &retained.fingerprint {
        StateIdFingerprint::Absent => (None, None, None),
        StateIdFingerprint::Canonical { witness, bytes, .. } => (Some(*witness), None, Some(bytes.as_slice())),
        StateIdFingerprint::Corrupt {
            state,
            temporary,
            bytes,
            ..
        } => (*state, *temporary, bytes.as_deref()),
    };
    revalidate_optional_named_entry(usr, b".stateID", expected_state, &state_path, budget)?;
    revalidate_optional_named_entry(usr, b".cast-state-id.tmp", expected_temporary, &temporary_path, budget)?;
    if let (Some(file), Some(witness)) = (&retained.file, expected_state) {
        require_witness(InodeWitness::read(file, &state_path)?, witness, &state_path)?;
        if let Some(expected) = expected_bytes {
            budget.operation(&state_path)?;
            let mut actual = vec![0; expected.len()];
            read_exact_at(file, &mut actual, &state_path)?;
            if actual != expected {
                return Err(CaptureError::InodeChanged { path: state_path });
            }
        }
    }
    if let (Some(file), Some(witness)) = (&retained.temporary, expected_temporary) {
        require_witness(InodeWitness::read(file, &temporary_path)?, witness, &temporary_path)?;
    }
    Ok(())
}

fn revalidate_optional_named_entry(
    parent: &File,
    name: &[u8],
    expected: Option<InodeWitness>,
    path: &Path,
    budget: &mut Budget,
) -> Result<(), CaptureError> {
    let actual = open_optional_path(parent, cstring(name)?.as_c_str(), path, budget)?;
    match (actual, expected) {
        (None, None) => Ok(()),
        (Some(file), Some(expected)) => require_witness(InodeWitness::read(&file, path)?, expected, path),
        (None, Some(_)) | (Some(_), None) => Err(CaptureError::InodeChanged { path: path.to_owned() }),
    }
}

fn revalidate_root_abi(
    retained: &RetainedRootAbi,
    directory: &File,
    path: &Path,
    budget: &mut Budget,
) -> Result<(), CaptureError> {
    for ((name, expected_target), retained_link) in ROOT_ABI_LINKS.into_iter().zip(&retained.links) {
        let mut temporary = name.to_vec();
        temporary.extend_from_slice(b".next");
        if name_exists(
            directory,
            cstring(&temporary)?.as_c_str(),
            &path.join(os(&temporary)),
            budget,
        )? {
            return Err(CaptureError::RootAbiTemporary {
                path: path.join(os(&temporary)),
            });
        }
        let link_path = path.join(os(name));
        match retained_link {
            None => {
                if name_exists(directory, cstring(name)?.as_c_str(), &link_path, budget)? {
                    return Err(CaptureError::InodeChanged { path: link_path });
                }
            }
            Some(link) => {
                revalidate_named_entry(directory, name, link.fingerprint.witness, &link_path, budget)?;
                require_witness(
                    InodeWitness::read(&link.file, &link_path)?,
                    link.fingerprint.witness,
                    &link_path,
                )?;
                if read_link(&link.file, &link_path, budget)? != expected_target {
                    return Err(CaptureError::InodeChanged { path: link_path });
                }
            }
        }
    }
    Ok(())
}
