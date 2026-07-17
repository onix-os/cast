use crate::transition_journal::{BootId, MountNamespaceIdentity};

use super::*;

mod masks;
mod parent_rebind;
mod projection;
mod semantic_fields;

const CANDIDATE_TOKEN: &str = "candidate-token";
const PREVIOUS_TOKEN: &str = "previous-token";

fn witness(seed: u64) -> InodeWitness {
    InodeWitness {
        device: 7,
        inode: seed,
        mode: nix::libc::S_IFDIR | 0o700,
        owner: 1_000,
        group: 1_000,
        links: 3,
        length: 4_096,
        modified_seconds: 100,
        modified_nanoseconds: 101,
        changed_seconds: 102,
        changed_nanoseconds: 103,
    }
}

fn usr(token: &str, location: TreeLocation, inode: u64) -> UsrFingerprint {
    let mut marker = witness(inode + 1_000);
    marker.mode = nix::libc::S_IFREG | 0o444;
    marker.links = 1;
    marker.length = 64;
    UsrFingerprint {
        location,
        token: token.to_owned(),
        directory: witness(inode),
        marker,
        state_id: StateIdFingerprint::Absent,
        runtime: RuntimeTreeIdentity {
            st_dev: 7,
            inode,
            mount_id: 9,
        },
    }
}

fn wrapper(name: &[u8], inode: u64) -> WrapperFingerprint {
    WrapperFingerprint {
        name: name.to_vec(),
        witness: witness(inode),
        role: TreeLocation::AmbientQuarantine(name.to_vec()),
        entries: Vec::new(),
        usr: None,
        slot: None,
    }
}

fn fingerprint(layout: UsrExchangeLayout) -> NamespaceFingerprint {
    let candidate = usr(CANDIDATE_TOKEN, TreeLocation::Live, 100);
    let previous = usr(PREVIOUS_TOKEN, TreeLocation::Staging, 200);
    let (mut live, mut staged) = match layout {
        UsrExchangeLayout::Post => (candidate, previous),
        UsrExchangeLayout::Pre => (previous, candidate),
    };
    live.location = TreeLocation::Live;
    staged.location = TreeLocation::Staging;
    let staging = WrapperFingerprint {
        name: b"staging".to_vec(),
        witness: witness(30),
        role: TreeLocation::Staging,
        entries: vec![(b"usr".to_vec(), staged.directory)],
        usr: Some(staged),
        slot: None,
    };
    NamespaceFingerprint {
        root: witness(10),
        roots: witness(20),
        quarantine: witness(40),
        epoch: RuntimeEpoch {
            boot_id: BootId::parse("01234567-89ab-4cde-8f01-23456789abcd").unwrap(),
            mount_namespace: MountNamespaceIdentity { st_dev: 50, inode: 51 },
        },
        live,
        root_abi: RootAbiFingerprint { links: Vec::new() },
        isolation_abi: RootAbiFingerprint { links: Vec::new() },
        roots_entries: vec![staging, wrapper(b"isolation", 31)],
        quarantine_entries: vec![wrapper(b"ambient", 41)],
        new_state_target_residue: None,
    }
}

fn project(fingerprint: &NamespaceFingerprint) -> ProjectedReverseNamespace {
    ProjectedReverseNamespace::from_fingerprint(fingerprint, CANDIDATE_TOKEN, PREVIOUS_TOKEN).unwrap()
}

fn staging(fingerprint: &NamespaceFingerprint) -> &WrapperFingerprint {
    fingerprint
        .roots_entries
        .iter()
        .find(|wrapper| wrapper.name == b"staging")
        .unwrap()
}

fn staging_mut(fingerprint: &mut NamespaceFingerprint) -> &mut WrapperFingerprint {
    fingerprint
        .roots_entries
        .iter_mut()
        .find(|wrapper| wrapper.name == b"staging")
        .unwrap()
}

fn tree_for_token_mut<'a>(fingerprint: &'a mut NamespaceFingerprint, token: &str) -> &'a mut UsrFingerprint {
    if fingerprint.live.token == token {
        return &mut fingerprint.live;
    }
    fingerprint
        .roots_entries
        .iter_mut()
        .chain(&mut fingerprint.quarantine_entries)
        .filter_map(|wrapper| wrapper.usr.as_mut())
        .find(|tree| tree.token == token)
        .unwrap()
}

fn allow_exchange_timestamps(fingerprint: &mut NamespaceFingerprint) {
    fingerprint.root.modified_seconds += 10;
    fingerprint.root.modified_nanoseconds += 10;
    fingerprint.root.changed_seconds += 10;
    fingerprint.root.changed_nanoseconds += 10;

    let staging = staging_mut(fingerprint);
    staging.witness.modified_seconds += 20;
    staging.witness.modified_nanoseconds += 20;
    staging.witness.changed_seconds += 20;
    staging.witness.changed_nanoseconds += 20;

    fingerprint.live.directory.changed_seconds += 30;
    fingerprint.live.directory.changed_nanoseconds += 30;
    let staging = staging_mut(fingerprint);
    let staged = staging.usr.as_mut().unwrap();
    staged.directory.changed_seconds += 40;
    staged.directory.changed_nanoseconds += 40;
    staging.entries[0].1 = staged.directory;
}
