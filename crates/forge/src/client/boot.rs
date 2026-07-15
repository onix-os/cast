// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Boot management integration in Cast

use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    str::FromStr,
    vec,
};

use blsforme::{
    CmdlineEntry, Entry, Schema,
    bootloader::systemd_boot,
    os_release::{self, OsRelease},
};
use fnmatch::Pattern;
use fs_err as fs;
use itertools::Itertools;
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};
use thiserror::{self, Error};

use crate::{Installation, State, db, package::Id, state::Id as StateId};

use super::Client;

const MAX_ROLLBACK_STATES: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProjectionSyncOutcome {
    NotApplicable,
    NotApplied,
    Applied,
    Ambiguous,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProjectionSyncFaultPoint {
    BeforeSideEffects,
    AfterSideEffects,
}

#[cfg(test)]
std::thread_local! {
    static BOOT_PROJECTION_SYNC: std::cell::RefCell<std::collections::VecDeque<Box<dyn FnOnce(&[StateId])>>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
    static PROJECTION_SYNC_FAULT: std::cell::Cell<Option<ProjectionSyncFaultPoint>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn arm_boot_projection_sync(hook: impl FnOnce(&[StateId]) + 'static) {
    BOOT_PROJECTION_SYNC.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.len() < 4, "too many boot projection hooks armed");
        slot.push_back(Box::new(hook));
    });
}

#[cfg(test)]
pub(crate) fn arm_projection_sync_fault(point: ProjectionSyncFaultPoint) {
    PROJECTION_SYNC_FAULT.with(|slot| assert!(slot.replace(Some(point)).is_none(), "boot projection fault armed"));
}

#[cfg(test)]
fn run_boot_projection_sync(projected: &[StateId]) -> bool {
    BOOT_PROJECTION_SYNC.with(|slot| {
        let Some(hook) = slot.borrow_mut().pop_front() else {
            return false;
        };
        hook(projected);
        true
    })
}

#[cfg(test)]
fn projection_sync_checkpoint(point: ProjectionSyncFaultPoint) -> Result<(), Error> {
    if PROJECTION_SYNC_FAULT.with(|slot| slot.get()) == Some(point) {
        PROJECTION_SYNC_FAULT.with(|slot| slot.set(None));
        return Err(match point {
            ProjectionSyncFaultPoint::BeforeSideEffects => Error::InjectedProjectionBeforeSideEffects,
            ProjectionSyncFaultPoint::AfterSideEffects => Error::InjectedProjectionAfterSideEffects,
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("blsforme")]
    Blsforme(#[from] blsforme::Error),

    #[error("sd_boot")]
    SdBoot(#[from] systemd_boot::interface::Error),

    #[error("layoutdb")]
    Client(#[from] db::layout::Error),

    #[error("io")]
    IO(#[from] io::Error),

    #[error("os_info")]
    OsInfo(#[from] os_info::Error),

    #[error("os_release")]
    OsRelease(#[from] os_release::Error),

    /// fnmatch pattern compilation for boot, etc.
    #[error("fnmatch pattern")]
    Pattern(#[from] fnmatch::Error),

    #[error("incomplete kernel tree")]
    IncompleteKernel(String),

    #[error("cannot exclude active boot state {0}")]
    ExcludedHead(i32),

    #[error("exact prune boot projection was skipped without being applied")]
    ExactProjectionSkipped,

    #[cfg(test)]
    #[error("injected boot projection fault before side effects")]
    InjectedProjectionBeforeSideEffects,

    #[cfg(test)]
    #[error("injected boot projection fault after side effects")]
    InjectedProjectionAfterSideEffects,
}

impl Error {
    pub(crate) fn projection_sync_outcome(&self) -> ProjectionSyncOutcome {
        #[cfg(test)]
        if matches!(self, Self::InjectedProjectionBeforeSideEffects) {
            return ProjectionSyncOutcome::NotApplied;
        }
        ProjectionSyncOutcome::Ambiguous
    }
}

/// Simple mapping type for kernel discovery paths, retaining the layout reference
#[derive(Debug)]
struct KernelCandidate {
    path: PathBuf,
    _layout: StonePayloadLayoutRecord,
}

impl AsRef<Path> for KernelCandidate {
    fn as_ref(&self) -> &Path {
        self.path.as_path()
    }
}

/// From a given set of input paths, produce a set of match pairs
/// This is applied against the given system root
fn kernel_files_from_state<'a>(
    layouts: &'a [(Id, StonePayloadLayoutRecord)],
    pattern: &'a Pattern,
) -> Vec<KernelCandidate> {
    let mut kernel_entries = vec![];

    for (_, path) in layouts.iter() {
        match &path.file {
            StonePayloadLayoutFile::Regular(_, target) => {
                if pattern.match_path(target).is_some() {
                    kernel_entries.push(KernelCandidate {
                        path: PathBuf::from("usr").join(target),
                        _layout: path.to_owned(),
                    });
                }
            }
            StonePayloadLayoutFile::Symlink(_, target) if pattern.match_path(target).is_some() => {
                kernel_entries.push(KernelCandidate {
                    path: PathBuf::from("usr").join(target),
                    _layout: path.to_owned(),
                });
            }
            _ => {}
        }
    }

    kernel_entries
}

/// Find bootloader assets in the new state
fn boot_files_from_new_state<'a>(
    install: &Installation,
    layouts: &'a [(Id, StonePayloadLayoutRecord)],
    pattern: &'a Pattern,
) -> Vec<PathBuf> {
    let mut rets = vec![];

    for (_, path) in layouts.iter() {
        if let StonePayloadLayoutFile::Regular(_, target) = &path.file
            && pattern.match_path(target).is_some()
        {
            rets.push(install.root.join("usr").join(target));
        }
    }

    rets
}

/// Grab all layouts for the provided state, mapped to package id
fn layouts_for_state(client: &Client, state: &State) -> Result<Vec<(Id, StonePayloadLayoutRecord)>, db::Error> {
    client.layout_db.query(state.selections.iter().map(|s| &s.package))
}

/// Select the bounded rollback-state IDs for one active head.
///
/// An explicit immediate previous state is always pinned first. Remaining
/// capacity is filled with the most recently created states without assuming
/// that a rollback head has the greatest numeric ID: activating an older state
/// must not make newer, still-valid alternatives disappear on the next
/// standalone boot synchronization.
fn select_rollback_state_ids<T: Ord>(
    head: StateId,
    immediate_previous: Option<StateId>,
    candidates: impl IntoIterator<Item = (StateId, T)>,
) -> Vec<StateId> {
    select_rollback_state_ids_excluding(head, immediate_previous, candidates, &BTreeSet::new())
}

fn select_rollback_state_ids_excluding<T: Ord>(
    head: StateId,
    immediate_previous: Option<StateId>,
    candidates: impl IntoIterator<Item = (StateId, T)>,
    excluded: &BTreeSet<StateId>,
) -> Vec<StateId> {
    let immediate_previous = immediate_previous.filter(|id| *id != head && !excluded.contains(id));
    let mut selected = Vec::with_capacity(MAX_ROLLBACK_STATES);
    if let Some(id) = immediate_previous {
        selected.push(id);
    }

    let remaining = MAX_ROLLBACK_STATES - selected.len();
    selected.extend(
        candidates
            .into_iter()
            .filter(|(id, _)| *id != head && Some(*id) != immediate_previous && !excluded.contains(id))
            .sorted_by(|(left_id, left_created), (right_id, right_created)| {
                right_created.cmp(left_created).then_with(|| right_id.cmp(left_id))
            })
            .take(remaining)
            .map(|(id, _)| id),
    );
    selected
}

/// Resolve every selected rollback state without silently reducing the boot
/// set when one database lookup fails. The transition-supplied predecessor is
/// already an authenticated in-memory value and therefore needs no second
/// lookup.
fn resolve_rollback_states<T: Clone, E>(
    ids: impl IntoIterator<Item = StateId>,
    immediate_previous: Option<(StateId, &T)>,
    mut load: impl FnMut(StateId) -> Result<T, E>,
) -> Result<Vec<T>, E> {
    ids.into_iter()
        .map(|id| match immediate_previous {
            Some((previous_id, previous)) if previous_id == id => Ok((*previous).clone()),
            _ => load(id),
        })
        .collect()
}

fn rollback_states_excluding(
    client: &Client,
    state: &State,
    immediate_previous: Option<&State>,
    excluded: &BTreeSet<StateId>,
) -> Result<Vec<State>, db::Error> {
    let candidates = client.state_db.list_ids()?;
    let ids = if excluded.is_empty() {
        select_rollback_state_ids(state.id, immediate_previous.map(|previous| previous.id), candidates)
    } else {
        select_rollback_state_ids_excluding(
            state.id,
            immediate_previous.map(|previous| previous.id),
            candidates,
            excluded,
        )
    };

    resolve_rollback_states(ids, immediate_previous.map(|previous| (previous.id, previous)), |id| {
        client.state_db.get(id)
    })
}

fn boot_state_is_eligible(state_id: StateId, excluded: &BTreeSet<StateId>, sysroot_exists: bool) -> bool {
    !excluded.contains(&state_id) && sysroot_exists
}

/// Generate a schema for the root
fn os_schema_for_root(root: &Path) -> Result<Schema, Error> {
    let os_info_path = root.join("usr").join("lib").join("os-info.json");
    let os_release_path = root.join("usr").join("lib").join("os-release");

    if os_info_path.exists() {
        let info = os_info::load_os_info_from_path(&os_info_path)?;
        Ok(Schema::OsInfo {
            os_info: Box::new(info),
        })
    } else {
        let os_release = fs::read_to_string(os_release_path)?;
        let os_release = OsRelease::from_str(&os_release)?;
        Ok(Schema::Blsforme {
            os_release: Box::new(os_release),
        })
    }
}

/// Synchronize boot metadata for `state`.
///
/// A transition should supply its immediate previous state so that state is
/// retained as the first rollback choice even when activating a lower ID.
/// Standalone synchronization and pruning may pass `None`.
pub fn synchronize(client: &Client, state: &State, immediate_previous: Option<&State>) -> Result<(), Error> {
    synchronize_excluding(client, state, immediate_previous, &BTreeSet::new()).map(|_| ())
}

/// Synchronize boot metadata while categorically excluding exact state IDs.
///
/// Pruning supplies its removal set before rollback selection, so excluded
/// candidates neither consume the bounded rollback capacity nor reappear if a
/// foreign inode races into a detached canonical archive name.
pub(crate) fn synchronize_excluding(
    client: &Client,
    state: &State,
    immediate_previous: Option<&State>,
    excluded: &BTreeSet<StateId>,
) -> Result<ProjectionSyncOutcome, Error> {
    if excluded.contains(&state.id) {
        return Err(Error::ExcludedHead(i32::from(state.id)));
    }
    let root = client.installation.root.clone();
    let is_native = root.to_string_lossy() == "/";
    // Create an appropriate configuration
    let config = blsforme::Configuration {
        root: if is_native {
            blsforme::Root::Native(root.clone())
        } else {
            blsforme::Root::Image(root.clone())
        },
        vfs: "/".into(),
    };

    // For the new/active state
    let head_layouts = layouts_for_state(client, state)?;
    let kernel_pattern = Pattern::from_str("lib/kernel/(version:*)/*")?;
    let systemd = Pattern::from_str("lib*/systemd/boot/efi/*.efi")?;
    let booty_bits = boot_files_from_new_state(&client.installation, &head_layouts, &systemd);

    let mut all_states = rollback_states_excluding(client, state, immediate_previous, excluded)?;

    let exact_prune_projection = !excluded.is_empty();
    // Ordinary synchronization keeps its historical no-bootloader no-op.
    // Pruning may not: if stale entries cannot be removed, it must fail before
    // exact state rows are deleted and the caller restores the prior layout.
    if booty_bits.is_empty() && !exact_prune_projection {
        return Ok(ProjectionSyncOutcome::NotApplicable);
    }

    let global_schema = os_schema_for_root(&root)?;

    // Grab the entries for the new state
    let mut all_kernels = vec![];
    all_states.insert(0, state.clone());
    for state in all_states.iter() {
        if excluded.contains(&state.id) {
            continue;
        }
        let layouts = layouts_for_state(client, state)?;
        let local_kernels = kernel_files_from_state(&layouts, &kernel_pattern);
        let mapped = global_schema.discover_system_kernels(local_kernels.into_iter())?;
        all_kernels.push((mapped, state.id));
    }

    // pipe all of our entries into blsforme
    let mut entries = all_kernels
        .iter()
        .flat_map(|&(ref kernels, state_id)| {
            let rootref = &root;
            kernels.iter().filter_map(move |k| {
                let sysroot = if state.id == state_id {
                    rootref.clone()
                } else {
                    client.installation.root_path(state_id.to_string()).to_owned()
                };

                if !boot_state_is_eligible(state_id, excluded, sysroot.exists()) {
                    return None;
                }

                let local_schema = os_schema_for_root(&sysroot).ok();
                let entry = Entry::new(k)
                    .with_cmdline(CmdlineEntry {
                        name: "---fstx---".to_owned(),
                        snippet: format!("cast.fstx={state_id}"),
                    })
                    .with_state_id(i32::from(state_id))
                    .with_sysroot(sysroot);

                match local_schema {
                    Some(schema) => Some(entry.with_schema(schema)),
                    None => Some(entry),
                }
            })
        })
        .collect::<Vec<_>>();

    #[cfg(test)]
    projection_sync_checkpoint(ProjectionSyncFaultPoint::BeforeSideEffects)?;

    #[cfg(test)]
    {
        let projected = all_kernels
            .iter()
            .filter_map(|(kernels, state_id)| {
                (!kernels.is_empty() && !excluded.contains(state_id)).then_some(*state_id)
            })
            .collect::<Vec<_>>();
        if run_boot_projection_sync(&projected) {
            projection_sync_checkpoint(ProjectionSyncFaultPoint::AfterSideEffects)?;
            return Ok(ProjectionSyncOutcome::Applied);
        }
    }

    for entry in entries.iter_mut() {
        if let Err(e) = entry.load_cmdline_snippets(&config) {
            log::warn!("Failed to load cmdline snippets: {e}");
        }
    }

    // no usable entries, lets get out of here.
    if entries.is_empty() && !exact_prune_projection {
        return Ok(ProjectionSyncOutcome::NotApplicable);
    }

    let manager = blsforme::Manager::new(&config)?
        .with_entries(entries.into_iter())
        .with_bootloader_assets(booty_bits);

    // Only allow mounting pre-sync for a native run
    if is_native {
        let _mounts = manager.mount_partitions()?;
        manager.sync(&global_schema)?;
    } else {
        manager.sync(&global_schema)?;
    }

    #[cfg(test)]
    projection_sync_checkpoint(ProjectionSyncFaultPoint::AfterSideEffects)?;

    Ok(ProjectionSyncOutcome::Applied)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::state::Id;

    use super::{
        MAX_ROLLBACK_STATES, boot_state_is_eligible, resolve_rollback_states, select_rollback_state_ids,
        select_rollback_state_ids_excluding,
    };

    fn id(value: i32) -> Id {
        Id::from(value)
    }

    #[test]
    fn reverse_id_activation_pins_the_newer_immediate_previous_state() {
        let selected = select_rollback_state_ids(
            id(7),
            Some(id(10)),
            [(id(10), 10), (id(8), 8), (id(6), 6), (id(5), 5), (id(4), 4)],
        );

        assert_eq!(selected, [id(10), id(8), id(6), id(5)]);
        assert_eq!(selected.len(), MAX_ROLLBACK_STATES);
    }

    #[test]
    fn standalone_sync_keeps_newer_ids_when_an_older_state_is_active() {
        let selected = select_rollback_state_ids(
            id(7),
            None,
            [(id(10), 10), (id(8), 8), (id(6), 6), (id(5), 5), (id(4), 4)],
        );

        assert_eq!(selected, [id(10), id(8), id(6), id(5)]);
        assert_eq!(selected.len(), MAX_ROLLBACK_STATES);
    }

    #[test]
    fn pinned_previous_is_deduplicated_and_counts_toward_capacity() {
        let selected = select_rollback_state_ids(
            id(10),
            Some(id(8)),
            [(id(9), 9), (id(8), 8), (id(7), 7), (id(6), 6), (id(5), 5)],
        );

        assert_eq!(selected, [id(8), id(9), id(7), id(6)]);
        assert_eq!(selected.len(), MAX_ROLLBACK_STATES);
        assert_eq!(selected.iter().filter(|&&selected| selected == id(8)).count(), 1);
    }

    #[test]
    fn selected_state_lookup_failure_aborts_instead_of_reducing_the_boot_set() {
        let pinned = String::from("pinned");
        let mut requested = Vec::new();
        let result = resolve_rollback_states([id(8), id(7)], Some((id(8), &pinned)), |state| {
            requested.push(state);
            Err::<String, _>("state database lookup failed")
        });

        assert_eq!(result, Err("state database lookup failed"));
        assert_eq!(requested, [id(7)]);
    }

    #[test]
    fn prune_exclusions_are_applied_before_bounded_rollback_selection() {
        let excluded = BTreeSet::from([id(10), id(8)]);
        let selected = select_rollback_state_ids_excluding(
            id(11),
            Some(id(10)),
            [(id(10), 10), (id(9), 9), (id(8), 8), (id(7), 7), (id(6), 6), (id(5), 5)],
            &excluded,
        );

        assert_eq!(selected, [id(9), id(7), id(6), id(5)]);
        assert_eq!(selected.len(), MAX_ROLLBACK_STATES);
    }

    #[test]
    fn excluded_state_is_ineligible_even_when_its_canonical_path_exists() {
        let excluded = BTreeSet::from([id(8)]);

        assert!(!boot_state_is_eligible(id(8), &excluded, true));
        assert!(boot_state_is_eligible(id(7), &excluded, true));
        assert!(!boot_state_is_eligible(id(7), &excluded, false));
    }
}

pub fn print_status(installation: &Installation) -> Result<(), Error> {
    fn display_optional_path(path: Option<&Path>) -> std::path::Display<'_> {
        path.unwrap_or_else(|| "none".as_ref()).display()
    }

    let root = &installation.root;
    let is_native = root == Path::new("/");
    let config = blsforme::Configuration {
        root: if is_native {
            blsforme::Root::Native(root.clone())
        } else {
            blsforme::Root::Image(root.clone())
        },
        vfs: "/".into(),
    };

    let manager = blsforme::Manager::new(&config)?;
    match manager.boot_environment().firmware {
        blsforme::Firmware::Uefi => {
            let esp = display_optional_path(manager.boot_environment().esp());
            let xbootldr = display_optional_path(manager.boot_environment().xbootldr());
            println!("ESP            : {esp}");
            println!("XBOOTLDR       : {xbootldr}");
            if is_native && let Ok(bootloader) = systemd_boot::interface::BootLoaderInterface::new(&config.vfs) {
                let v = bootloader.get_ucs2_string(systemd_boot::interface::VariableName::Info)?;
                println!("Bootloader     : {v}");
            }
        }
        blsforme::Firmware::Bios => {
            let boot = display_optional_path(manager.boot_environment().boot_partition());
            println!("BOOT           : {boot}");
        }
    }

    println!("Global cmdline : {:?}", manager.cmdline());

    Ok(())
}
