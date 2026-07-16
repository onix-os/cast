use super::*;

pub(super) fn inspect_roots(
    roots: &File,
    path: &Path,
    record: &TransitionRecord,
    budget: &mut Budget,
) -> Result<Vec<RetainedWrapper>, CaptureError> {
    let names = directory_names(roots, path, MAX_NAMESPACE_ENTRIES, budget)?;
    let mut wrappers = Vec::new();
    for name in names {
        let role = classify_root_name(&name, record)?;
        let location = match role {
            RootEntryRole::Staging => TreeLocation::Staging,
            RootEntryRole::Isolation => {
                let directory = open_directory(roots, cstring(&name)?.as_c_str(), &path.join(os(&name)), budget)?;
                let isolation_path = path.join(os(&name));
                let witness = controlled_directory_witness(&directory, &isolation_path)?;
                let mut entries = Vec::new();
                let mut isolation_scaffolds = Vec::new();
                for child in directory_names(&directory, &isolation_path, MAX_WRAPPER_ENTRIES, budget)? {
                    let child_path = isolation_path.join(os(&child));
                    if ROOT_ABI_LINKS.iter().any(|(allowed, _)| *allowed == child) {
                        let retained =
                            open_optional_path(&directory, cstring(&child)?.as_c_str(), &child_path, budget)?
                                .ok_or_else(|| CaptureError::InodeChanged {
                                    path: child_path.clone(),
                                })?;
                        entries.push((child, InodeWitness::read(&retained, &child_path)?));
                        continue;
                    }
                    if ISOLATION_SCAFFOLD_DIRECTORIES.contains(&child.as_slice()) {
                        let retained = open_directory(&directory, cstring(&child)?.as_c_str(), &child_path, budget)?;
                        let scaffold_witness = controlled_directory_witness(&retained, &child_path)?;
                        if let Some(nested) = directory_names(&retained, &child_path, MAX_WRAPPER_ENTRIES, budget)?
                            .into_iter()
                            .next()
                        {
                            return Err(CaptureError::UnexpectedWrapperEntry {
                                wrapper: child_path,
                                name: nested,
                            });
                        }
                        entries.push((child, scaffold_witness));
                        isolation_scaffolds.push(RetainedIsolationScaffold {
                            directory: retained,
                            path: child_path,
                            witness: scaffold_witness,
                        });
                        continue;
                    }
                    return Err(CaptureError::UnexpectedIsolationEntry { name: child });
                }
                wrappers.push(RetainedWrapper {
                    directory,
                    fingerprint: WrapperFingerprint {
                        name,
                        witness,
                        role: TreeLocation::AmbientQuarantine(b"isolation".to_vec()),
                        entries,
                        usr: None,
                        slot: None,
                    },
                    isolation_scaffolds,
                    usr: None,
                    slot: None,
                });
                continue;
            }
            RootEntryRole::State(state) => TreeLocation::State(state),
            RootEntryRole::ArchivedCandidateParking { state, token, index } => {
                TreeLocation::ArchivedCandidateParking { state, token, index }
            }
            RootEntryRole::PreviousParking { state, index } => TreeLocation::PreviousParking { state, index },
        };
        wrappers.push(inspect_wrapper(roots, path, name, location, record, budget)?);
    }
    if !wrappers.iter().any(|entry| entry.fingerprint.name == b"staging") {
        return Err(CaptureError::FixedWrapperMissing { name: "staging" });
    }
    if !wrappers.iter().any(|entry| entry.fingerprint.name == b"isolation") {
        return Err(CaptureError::FixedWrapperMissing { name: "isolation" });
    }
    Ok(wrappers)
}

pub(super) fn inspect_quarantine(
    quarantine: &File,
    path: &Path,
    record: &TransitionRecord,
    budget: &mut Budget,
) -> Result<Vec<RetainedWrapper>, CaptureError> {
    let names = directory_names(quarantine, path, MAX_NAMESPACE_ENTRIES, budget)?;
    names
        .into_iter()
        .map(|name| {
            let role = classify_quarantine_name(&name, record)?;
            let location = match role {
                QuarantineEntryRole::Transition => TreeLocation::TransitionQuarantine,
                QuarantineEntryRole::ActiveReblitWrapper { state, index } => {
                    TreeLocation::ActiveReblitWrapper { state, index }
                }
                QuarantineEntryRole::Ambient => TreeLocation::AmbientQuarantine(name.clone()),
            };
            inspect_wrapper(quarantine, path, name, location, record, budget)
        })
        .collect()
}

fn inspect_wrapper(
    parent: &File,
    parent_path: &Path,
    name: Vec<u8>,
    role: TreeLocation,
    record: &TransitionRecord,
    budget: &mut Budget,
) -> Result<RetainedWrapper, CaptureError> {
    let encoded = cstring(&name)?;
    let path = parent_path.join(os(&name));
    let directory = open_directory(parent, &encoded, &path, budget)?;
    let witness = controlled_directory_witness(&directory, &path)?;
    let names = directory_names(&directory, &path, MAX_WRAPPER_ENTRIES, budget)?;
    let mut entries = Vec::new();
    let mut usr = None;
    let mut slot = None;
    for child in names {
        let child_path = path.join(os(&child));
        if child == b"usr" {
            usr = inspect_usr(&directory, c"usr", child_path, role.clone(), budget)?;
            let tree = usr
                .as_ref()
                .ok_or_else(|| CaptureError::RequiredTreeMissing { location: role.clone() })?;
            entries.push((child, tree.fingerprint.directory));
            continue;
        }
        if child.starts_with(b".cast-state-slot-") {
            if slot.is_some() {
                return Err(CaptureError::DuplicateSlotLink { path });
            }
            let retained = inspect_slot_link(&directory, &child, &child_path, budget)?;
            entries.push((child.clone(), retained.fingerprint.witness));
            slot = Some(retained);
            continue;
        }
        return Err(CaptureError::UnexpectedWrapperEntry {
            wrapper: path,
            name: child,
        });
    }
    validate_wrapper_shape(record, &role, &path, usr.as_ref(), slot.as_ref())?;
    let fingerprint = WrapperFingerprint {
        name,
        witness,
        role,
        entries,
        usr: usr.as_ref().map(|tree| tree.fingerprint.clone()),
        slot: slot.as_ref().map(|link| link.fingerprint.clone()),
    };
    Ok(RetainedWrapper {
        directory,
        fingerprint,
        isolation_scaffolds: Vec::new(),
        usr,
        slot,
    })
}

pub(super) fn inspect_usr(
    parent: &File,
    name: &CStr,
    path: PathBuf,
    location: TreeLocation,
    budget: &mut Budget,
) -> Result<Option<RetainedUsr>, CaptureError> {
    let Some(directory) = open_optional_directory(parent, name, &path, budget)? else {
        return Ok(None);
    };
    let store = TreeMarkerStore::open(&directory, &path).map_err(CaptureError::TreeMarker)?;
    let directory_witness = safe_usr_witness(&store, &path, budget)?;
    let marker = store.read_for_transition_recovery().map_err(CaptureError::TreeMarker)?;
    let marker_path = path.join(".cast-tree-id");
    let marker_file = open_file(&directory, c".cast-tree-id", &marker_path, budget)?;
    let marker_witness = InodeWitness::read(&marker_file, &marker_path)?;
    let state_id = inspect_state_id(&directory, &path, budget)?;
    let runtime = RuntimeTreeIdentity::capture_directory(&directory).map_err(|source| CaptureError::RuntimeTree {
        path: path.clone(),
        source,
    })?;
    let fingerprint = UsrFingerprint {
        location,
        token: marker.token().as_str().to_owned(),
        directory: directory_witness,
        marker: marker_witness,
        state_id: state_id.fingerprint.clone(),
        runtime,
    };
    Ok(Some(RetainedUsr {
        store,
        marker,
        state_id,
        fingerprint,
    }))
}

fn inspect_state_id(usr: &File, path: &Path, budget: &mut Budget) -> Result<RetainedStateId, CaptureError> {
    const MAX_RETAINED_STATE_ID_BYTES: usize = 64;

    let temporary_path = path.join(".cast-state-id.tmp");
    let temporary = open_optional_path(usr, c".cast-state-id.tmp", &temporary_path, budget)?;
    let temporary_witness = temporary
        .as_ref()
        .map(|file| InodeWitness::read(file, &temporary_path))
        .transpose()?;
    let state_path = path.join(".stateID");
    let Some(pinned) = open_optional_path(usr, c".stateID", &state_path, budget)? else {
        let fingerprint = match temporary_witness {
            Some(temporary) => StateIdFingerprint::Corrupt {
                reason: StateIdCorruption::TemporaryName,
                state: None,
                temporary: Some(temporary),
                bytes: None,
            },
            None => StateIdFingerprint::Absent,
        };
        return Ok(RetainedStateId {
            file: None,
            temporary,
            fingerprint,
        });
    };
    let witness = InodeWitness::read(&pinned, &state_path)?;
    let length = usize::try_from(witness.length).unwrap_or(usize::MAX);
    if witness.kind() != nix::libc::S_IFREG
        || witness.owner != effective_uid()
        || witness.mode & 0o7777 != 0o644
        || witness.links != 1
        || length > MAX_RETAINED_STATE_ID_BYTES
    {
        return Ok(RetainedStateId {
            file: Some(pinned),
            temporary,
            fingerprint: StateIdFingerprint::Corrupt {
                reason: if temporary_witness.is_some() {
                    StateIdCorruption::TemporaryName
                } else {
                    StateIdCorruption::UnsafeMetadata
                },
                state: Some(witness),
                temporary: temporary_witness,
                bytes: None,
            },
        });
    }
    let file = open_file(usr, c".stateID", &state_path, budget)?;
    if InodeWitness::read(&file, &state_path)? != witness {
        return Err(CaptureError::InodeChanged { path: state_path });
    }
    budget.operation(&state_path)?;
    let mut bytes = vec![0; length];
    read_exact_at(&file, &mut bytes, &state_path)?;
    if InodeWitness::read(&file, &state_path)? != witness {
        return Err(CaptureError::InodeChanged { path: state_path });
    }
    let state = parse_positive_decimal(&bytes);
    let fingerprint = if temporary_witness.is_some() {
        StateIdFingerprint::Corrupt {
            reason: StateIdCorruption::TemporaryName,
            state: Some(witness),
            temporary: temporary_witness,
            bytes: Some(bytes),
        }
    } else if let Some(state) = state {
        StateIdFingerprint::Canonical { state, witness, bytes }
    } else {
        StateIdFingerprint::Corrupt {
            reason: StateIdCorruption::InvalidContent,
            state: Some(witness),
            temporary: None,
            bytes: Some(bytes),
        }
    };
    Ok(RetainedStateId {
        file: Some(file),
        temporary,
        fingerprint,
    })
}

fn inspect_slot_link(
    wrapper: &File,
    name: &[u8],
    path: &Path,
    budget: &mut Budget,
) -> Result<RetainedSlotLink, CaptureError> {
    let (state, token) =
        parse_slot_name(name).ok_or_else(|| CaptureError::InvalidSlotName { path: path.to_owned() })?;
    let encoded = cstring(name)?;
    let file = open_file(wrapper, &encoded, path, budget)?;
    let witness = InodeWitness::read(&file, path)?;
    if witness.kind() != nix::libc::S_IFREG
        || witness.owner != effective_uid()
        || witness.mode & 0o7777 != 0o444
        || witness.links != 2
        || witness.length != crate::tree_marker::TREE_MARKER_FRAME_LENGTH as u64
    {
        return Err(CaptureError::UnsafeSlotLink { path: path.to_owned() });
    }
    Ok(RetainedSlotLink {
        file,
        name: name.to_vec(),
        path: path.to_owned(),
        fingerprint: SlotFingerprint { state, token, witness },
    })
}

pub(super) fn authenticate_slot_links(
    record: &TransitionRecord,
    live: &RetainedUsr,
    roots: &mut [RetainedWrapper],
    quarantine: &mut [RetainedWrapper],
) -> Result<(), CaptureError> {
    let slots = roots
        .iter()
        .chain(quarantine.iter())
        .filter_map(|wrapper| wrapper.slot.as_ref().map(|slot| (slot, &wrapper.fingerprint.role)))
        .collect::<Vec<_>>();
    let mut trees = Vec::new();
    trees.push(live);
    trees.extend(
        roots
            .iter()
            .chain(quarantine.iter())
            .filter_map(|wrapper| wrapper.usr.as_ref()),
    );
    for tree in &trees {
        let transition_role = if tree.fingerprint.token == record.candidate.tree_token.as_str() {
            TransitionTreeRole::Candidate
        } else if tree.fingerprint.token == record.previous.tree_token.as_str() {
            TransitionTreeRole::Previous
        } else if let TreeLocation::State(state) = &tree.fingerprint.location {
            TransitionTreeRole::AmbientState(*state)
        } else {
            TransitionTreeRole::Foreign
        };
        let matches = slots
            .iter()
            .filter(|(slot, _)| slot.fingerprint.token == tree.fingerprint.token)
            .copied()
            .collect::<Vec<_>>();
        if tree.marker.needs_slot_link_authorization() {
            let [(slot, role)] = matches.as_slice() else {
                return Err(CaptureError::SlotAuthorizationCount {
                    token: tree.fingerprint.token.clone(),
                    actual: matches.len(),
                });
            };
            let expected_state = match transition_role {
                TransitionTreeRole::Candidate => record.candidate.id,
                TransitionTreeRole::Previous => record.previous.id,
                TransitionTreeRole::AmbientState(state) => Some(state),
                TransitionTreeRole::Foreign => None,
            };
            if expected_state != Some(slot.fingerprint.state)
                || !role_owns_transition_slot(record, transition_role, role, slot.fingerprint.state)
            {
                return Err(CaptureError::SlotWrongTransitionState {
                    token: tree.fingerprint.token.clone(),
                    actual: slot.fingerprint.state,
                    expected: expected_state,
                });
            }
            tree.marker
                .require_recovery_slot_link_candidate(&slot.file, &slot.path)
                .map_err(CaptureError::TreeMarker)?;
            tree.marker
                .authorize_recovered_slot_link()
                .map_err(CaptureError::TreeMarker)?;
            tree.marker.revalidate(&tree.store).map_err(CaptureError::TreeMarker)?;
        } else if !matches.is_empty() {
            return Err(CaptureError::OrphanSlotLink {
                token: tree.fingerprint.token.clone(),
            });
        }
    }
    for (slot, _) in slots {
        if !trees
            .iter()
            .any(|tree| tree.fingerprint.token == slot.fingerprint.token)
        {
            return Err(CaptureError::OrphanSlotLink {
                token: slot.fingerprint.token.clone(),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum TransitionTreeRole {
    Candidate,
    Previous,
    AmbientState(i32),
    Foreign,
}

fn role_owns_transition_slot(
    record: &TransitionRecord,
    role: TransitionTreeRole,
    location: &TreeLocation,
    state: i32,
) -> bool {
    match role {
        TransitionTreeRole::Candidate => {
            matches!(location, TreeLocation::State(actual) if *actual == state)
                || (record.operation == Operation::ActivateArchived
                    && matches!(
                        location,
                        TreeLocation::ArchivedCandidateParking { state: actual, token, .. }
                            if *actual == state && token == record.candidate.tree_token.as_str()
                    ))
        }
        TransitionTreeRole::Previous => {
            matches!(location, TreeLocation::State(actual) if *actual == state)
                || (record.operation != Operation::ActiveReblit
                    && matches!(location, TreeLocation::PreviousParking { state: actual, .. } if *actual == state))
                || matches!(
                    location,
                    TreeLocation::ArchivedCandidateParking { state: actual, token, .. }
                        if *actual == state && token == record.previous.tree_token.as_str()
                )
        }
        TransitionTreeRole::AmbientState(expected) => {
            state == expected && matches!(location, TreeLocation::State(actual) if *actual == expected)
        }
        TransitionTreeRole::Foreign => false,
    }
}

pub(super) fn reject_duplicate_tree_tokens(
    live: &RetainedUsr,
    roots: &[RetainedWrapper],
    quarantine: &[RetainedWrapper],
) -> Result<(), CaptureError> {
    let mut counts = BTreeMap::<String, usize>::new();
    for tree in std::iter::once(live).chain(
        roots
            .iter()
            .chain(quarantine.iter())
            .filter_map(|wrapper| wrapper.usr.as_ref()),
    ) {
        *counts.entry(tree.fingerprint.token.clone()).or_default() += 1;
    }
    if let Some((token, count)) = counts.into_iter().find(|(_, count)| *count != 1) {
        Err(CaptureError::DuplicateTreeToken { token, count })
    } else {
        Ok(())
    }
}

fn validate_wrapper_shape(
    record: &TransitionRecord,
    role: &TreeLocation,
    path: &Path,
    usr: Option<&RetainedUsr>,
    slot: Option<&RetainedSlotLink>,
) -> Result<(), CaptureError> {
    if matches!(
        role,
        TreeLocation::ArchivedCandidateParking { .. } | TreeLocation::PreviousParking { .. }
    ) && usr.is_some()
    {
        return Err(CaptureError::ParkingWrapperContainsTree { path: path.to_owned() });
    }
    if let Some(slot) = slot {
        let role_state = match role {
            TreeLocation::State(state)
            | TreeLocation::ArchivedCandidateParking { state, .. }
            | TreeLocation::PreviousParking { state, .. } => Some(*state),
            _ => None,
        };
        if role_state != Some(slot.fingerprint.state) {
            return Err(CaptureError::SlotWrongWrapper { path: path.to_owned() });
        }
        if let Some(tree) = usr
            && tree.fingerprint.token != slot.fingerprint.token
        {
            return Err(CaptureError::SlotTokenMismatch { path: path.to_owned() });
        }
    }
    if let (TreeLocation::State(state), Some(tree)) = (role, usr)
        && tree.fingerprint.state_id.canonical_state() != Some(*state)
        && !marker_only_rearchived_candidate(record, *state, tree)
    {
        return Err(CaptureError::StateWrapperMismatch {
            path: path.to_owned(),
            expected: *state,
        });
    }
    Ok(())
}

fn marker_only_rearchived_candidate(record: &TransitionRecord, state: i32, tree: &RetainedUsr) -> bool {
    record.operation == Operation::ActivateArchived
        && record.candidate.id == Some(state)
        && tree.fingerprint.token == record.candidate.tree_token.as_str()
        && record
            .rollback
            .as_ref()
            .is_some_and(|rollback| rollback.candidate.disposition == AbortDisposition::Rearchive)
}
