use std::{
    fs::{self, Permissions},
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{Installation, state, test_support::private_installation_tempdir, tree_marker::TreeMarkerStore};

use super::*;

const HEAD_STATE_VALUE: i32 = 100;

pub(super) fn head_state() -> state::Id {
    state::Id::from(HEAD_STATE_VALUE)
}

pub(super) struct Fixture {
    _temporary: tempfile::TempDir,
    pub(super) installation: Installation,
    pub(super) head_usr: fs::File,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let temporary = private_installation_tempdir();
        create_exact_tree(&temporary.path().join("usr"), head_state());
        let installation = Installation::open(temporary.path(), None).unwrap();
        let head_usr = fs::File::open(temporary.path().join("usr")).unwrap();
        Self {
            _temporary: temporary,
            installation,
            head_usr,
        }
    }

    pub(super) fn archive_path(&self, state: state::Id) -> PathBuf {
        self.installation.root_path(state.to_string())
    }

    pub(super) fn create_archive(&self, state: state::Id) -> String {
        let wrapper = self.archive_path(state);
        fs::create_dir(&wrapper).unwrap();
        fs::set_permissions(&wrapper, Permissions::from_mode(0o700)).unwrap();
        create_exact_tree(&wrapper.join("usr"), state)
    }

    pub(super) fn prepare(
        &self,
        projected: &[state::Id],
    ) -> Result<PreparedActiveReblitBootStateRoots, ActiveReblitBootStateRootsError> {
        PreparedActiveReblitBootStateRoots::prepare(&self.installation, &self.head_usr, head_state(), projected)
    }

    pub(super) fn prepare_with_policy(
        &self,
        projected: &[state::Id],
        max_work: usize,
        timeout: Duration,
    ) -> Result<PreparedActiveReblitBootStateRoots, ActiveReblitBootStateRootsError> {
        PreparedActiveReblitBootStateRoots::prepare_for_test(
            &self.installation,
            &self.head_usr,
            head_state(),
            projected,
            max_work,
            timeout,
        )
    }
}

pub(super) fn state(value: i32) -> state::Id {
    state::Id::from(value)
}

pub(super) fn create_exact_tree(usr: &Path, state: state::Id) -> String {
    fs::create_dir(usr).unwrap();
    fs::set_permissions(usr, Permissions::from_mode(0o755)).unwrap();
    let state_id = usr.join(".stateID");
    fs::write(&state_id, state.to_string().as_bytes()).unwrap();
    fs::set_permissions(&state_id, Permissions::from_mode(0o644)).unwrap();
    let store = TreeMarkerStore::open_path(usr).unwrap();
    store
        .adopt_or_create_before_journal()
        .unwrap()
        .token()
        .as_str()
        .to_owned()
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct NamespaceEntry {
    relative: PathBuf,
    kind: u8,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    length: u64,
    device: u64,
    inode: u64,
    payload: Vec<u8>,
}

pub(super) fn namespace_snapshot(root: &Path) -> Vec<NamespaceEntry> {
    let mut entries = Vec::new();
    snapshot_entry(root, root, &mut entries);
    entries
}

fn snapshot_entry(root: &Path, path: &Path, output: &mut Vec<NamespaceEntry>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    let file_type = metadata.file_type();
    let (kind, payload) = if file_type.is_dir() {
        (0, Vec::new())
    } else if file_type.is_file() {
        (1, fs::read(path).unwrap())
    } else if file_type.is_symlink() {
        (2, fs::read_link(path).unwrap().as_os_str().as_bytes().to_vec())
    } else {
        (3, Vec::new())
    };
    output.push(NamespaceEntry {
        relative: path.strip_prefix(root).unwrap().to_owned(),
        kind,
        mode: metadata.mode() & 0o7777,
        owner: metadata.uid(),
        group: metadata.gid(),
        links: metadata.nlink(),
        length: metadata.len(),
        device: metadata.dev(),
        inode: metadata.ino(),
        payload,
    });
    if file_type.is_dir() {
        let mut children = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        children.sort();
        for child in children {
            snapshot_entry(root, &child, output);
        }
    }
}
