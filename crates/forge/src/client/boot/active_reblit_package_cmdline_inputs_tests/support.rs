use std::{collections::BTreeSet, os::unix::fs::PermissionsExt as _};

use astr::AStr;
use fs_err as fs;
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use super::*;
use crate::{
    Installation, State,
    client::{EMPTY_FILE_DIGEST, active_reblit_boot_inputs::ActiveReblitStoneBootInputsOutcome},
    db, package,
    state::Selection,
    test_support::private_installation_tempdir,
};

pub(super) struct PackageCmdlineFixture {
    _temporary: tempfile::TempDir,
    installation: Installation,
    state_db: db::state::Database,
    layout_db: db::layout::Database,
    pub(super) head: State,
}

impl PackageCmdlineFixture {
    pub(super) fn new(entries: impl IntoIterator<Item = (String, Vec<u8>)>) -> Self {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let state_db = db::state::Database::new(":memory:").unwrap();
        let layout_db = db::layout::Database::new(":memory:").unwrap();
        let selected = package::Id::from("package-cmdline-fixture".to_owned());
        let head = state_db
            .add(&[Selection::explicit(selected.clone())], Some("head"), None)
            .unwrap();

        let mut written = BTreeSet::new();
        let layouts = entries
            .into_iter()
            .map(|(path, bytes)| {
                let digest = xxhash_rust::xxh3::xxh3_128(&bytes);
                if digest != EMPTY_FILE_DIGEST && written.insert(digest) {
                    write_asset(&installation, digest, &bytes);
                }
                regular(digest, &path)
            })
            .collect::<Vec<_>>();
        layout_db
            .batch_add(layouts.iter().map(|layout| (&selected, layout)))
            .unwrap();

        Self {
            _temporary: temporary,
            installation,
            state_db,
            layout_db,
            head,
        }
    }

    pub(super) fn ready(&self) -> PreparedActiveReblitStoneBootInputs {
        match PreparedActiveReblitStoneBootInputs::prepare(
            &self.installation,
            &self.state_db,
            &self.layout_db,
            &self.head,
        )
        .unwrap()
        {
            ActiveReblitStoneBootInputsOutcome::Ready(stone) => stone,
            ActiveReblitStoneBootInputsOutcome::NotApplicable(reason) => {
                panic!("expected package-cmdline fixture to be applicable, got {reason:?}")
            }
        }
    }
}

pub(super) fn fixture_entries(cmdlines: impl IntoIterator<Item = (String, Vec<u8>)>) -> Vec<(String, Vec<u8>)> {
    let mut entries = vec![
        (
            "lib/systemd/boot/efi/systemd-bootx64.efi".to_owned(),
            b"fixture systemd bootloader".to_vec(),
        ),
        ("lib/kernel/6.12/vmlinuz".to_owned(), b"fixture 6.12 kernel".to_vec()),
        ("lib/kernel/6.6/vmlinuz".to_owned(), b"fixture 6.6 kernel".to_vec()),
    ];
    entries.extend(cmdlines);
    entries
}

pub(super) fn one_global(bytes: impl Into<Vec<u8>>) -> PackageCmdlineFixture {
    PackageCmdlineFixture::new(fixture_entries([(
        "lib/kernel/cmdline.d/10-global.cmdline".to_owned(),
        bytes.into(),
    )]))
}

fn regular(digest: u128, path: &str) -> StonePayloadLayoutRecord {
    StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o644,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(digest, AStr::from(path.to_owned())),
    }
}

fn write_asset(installation: &Installation, digest: u128, bytes: &[u8]) {
    let path = crate::client::cache::asset_path(installation, &format!("{digest:02x}"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
}
