use std::{
    fs::{self, File, Permissions},
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

use astr::AStr;
use serde_json::json;
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use crate::client::active_reblit_boot_inputs::ActiveReblitStoneBootInputsOutcome;
use crate::{
    Installation, State, client::EMPTY_FILE_DIGEST, db, package, state, state::Selection,
    test_support::private_installation_tempdir, transition_identity::PreparedActiveReblitBootStateRoots,
    tree_marker::TreeMarkerStore,
};

use super::*;

#[derive(Clone)]
pub(super) enum FixtureSchemaSource {
    OsInfo(Vec<u8>),
    Generated(Vec<u8>),
    MissingGenerated,
    NoBootAssets,
}

pub(super) struct Fixture {
    _temporary: tempfile::TempDir,
    pub(super) installation: Installation,
    pub(super) state_db: db::state::Database,
    pub(super) layout_db: db::layout::Database,
    pub(super) head: State,
    pub(super) histories: Vec<State>,
    pub(super) head_usr: File,
}

pub(super) struct PreparedFixtureSchemas {
    pub(super) stone: PreparedActiveReblitStoneBootInputs,
    pub(super) roots: PreparedActiveReblitBootStateRoots,
    pub(super) schemas: PreparedActiveReblitBootSchemas,
}

impl Fixture {
    pub(super) fn new(head_source: FixtureSchemaSource, history_sources: Vec<FixtureSchemaSource>) -> Self {
        let temporary = private_installation_tempdir();
        let state_db = db::state::Database::new(":memory:").unwrap();
        let layout_db = db::layout::Database::new(":memory:").unwrap();
        let head_package = package::Id::from("schema-head".to_owned());
        let head = state_db
            .add(&[Selection::explicit(head_package.clone())], Some("schema head"), None)
            .unwrap();
        let mut histories = Vec::with_capacity(history_sources.len());
        let mut history_packages = Vec::with_capacity(history_sources.len());
        for index in 0..history_sources.len() {
            let package = package::Id::from(format!("schema-history-{index}"));
            histories.push(
                state_db
                    .add(
                        &[Selection::explicit(package.clone())],
                        Some(&format!("schema history {index}")),
                        None,
                    )
                    .unwrap(),
            );
            history_packages.push(package);
        }

        create_exact_tree(&temporary.path().join("usr"), head.id);
        let installation = Installation::open(temporary.path(), None).unwrap();
        install_schema_source(&installation.root.join("usr"), &head_source);
        let head_usr = File::open(installation.root.join("usr")).unwrap();
        add_boot_layouts(&installation, &layout_db, &head_package, head.id, true, &head_source);

        for ((history, package), source) in histories.iter().zip(&history_packages).zip(&history_sources) {
            let wrapper = installation.root_path(history.id.to_string());
            fs::create_dir(&wrapper).unwrap();
            fs::set_permissions(&wrapper, Permissions::from_mode(0o700)).unwrap();
            create_exact_tree(&wrapper.join("usr"), history.id);
            install_schema_source(&wrapper.join("usr"), source);
            add_boot_layouts(&installation, &layout_db, package, history.id, false, source);
        }

        Self {
            _temporary: temporary,
            installation,
            state_db,
            layout_db,
            head,
            histories,
            head_usr,
        }
    }

    pub(super) fn generated_path(&self, state_id: state::Id) -> PathBuf {
        if state_id == self.head.id {
            self.installation.root.join("usr/lib/os-release")
        } else {
            self.installation
                .root_path(state_id.to_string())
                .join("usr/lib/os-release")
        }
    }

    pub(super) fn usr_path(&self, state_id: state::Id) -> PathBuf {
        if state_id == self.head.id {
            self.installation.root.join("usr")
        } else {
            self.installation.root_path(state_id.to_string()).join("usr")
        }
    }

    pub(super) fn write_generated(&self, state_id: state::Id, bytes: &[u8]) {
        let usr = self.usr_path(state_id);
        fs::create_dir_all(usr.join("lib")).unwrap();
        fs::set_permissions(usr.join("lib"), Permissions::from_mode(0o755)).unwrap();
        fs::write(usr.join("lib/os-release"), bytes).unwrap();
        fs::set_permissions(usr.join("lib/os-release"), Permissions::from_mode(0o644)).unwrap();
    }

    pub(super) fn exclude_history(&self, state_id: state::Id) {
        fs::remove_dir_all(self.installation.root_path(state_id.to_string())).unwrap();
    }

    pub(super) fn prepare(&self) -> Result<PreparedFixtureSchemas, ActiveReblitBootSchemaInputsError> {
        self.prepare_with_schema_policy(SCHEMA_POLICY)
    }

    pub(super) fn prepare_with_schema_policy(
        &self,
        policy: BootSchemaInputPolicy,
    ) -> Result<PreparedFixtureSchemas, ActiveReblitBootSchemaInputsError> {
        let stone = ready_stone(
            PreparedActiveReblitStoneBootInputs::prepare(
                &self.installation,
                &self.state_db,
                &self.layout_db,
                &self.head,
            )
            .unwrap(),
        );
        let roots = PreparedActiveReblitBootStateRoots::prepare(
            &self.installation,
            &self.head_usr,
            self.head.id,
            stone.state_ids(),
        )
        .unwrap();
        let revalidated = roots.revalidate(&self.installation).unwrap();
        let schemas = prepare_with_policy(&stone, &revalidated, policy)?;
        Ok(PreparedFixtureSchemas { stone, roots, schemas })
    }
}

impl PreparedFixtureSchemas {
    pub(super) fn revalidate(&self, fixture: &Fixture) -> Result<(), ActiveReblitBootSchemaInputsError> {
        let roots = self.roots.revalidate(&fixture.installation).unwrap();
        self.schemas.revalidate_sources(&self.stone, &roots)
    }
}

pub(super) fn ready_stone(outcome: ActiveReblitStoneBootInputsOutcome) -> PreparedActiveReblitStoneBootInputs {
    match outcome {
        ActiveReblitStoneBootInputsOutcome::Ready(stone) => stone,
        ActiveReblitStoneBootInputsOutcome::NotApplicable(reason) => {
            panic!("schema fixture must be bootable: {reason:?}")
        }
    }
}

fn create_exact_tree(usr: &Path, state_id: state::Id) {
    fs::create_dir(usr).unwrap();
    fs::set_permissions(usr, Permissions::from_mode(0o755)).unwrap();
    fs::write(usr.join(".stateID"), state_id.to_string()).unwrap();
    fs::set_permissions(usr.join(".stateID"), Permissions::from_mode(0o644)).unwrap();
    TreeMarkerStore::open_path(usr)
        .unwrap()
        .adopt_or_create_before_journal()
        .unwrap();
}

fn install_schema_source(usr: &Path, source: &FixtureSchemaSource) {
    if let FixtureSchemaSource::Generated(bytes) = source {
        fs::create_dir_all(usr.join("lib")).unwrap();
        fs::set_permissions(usr.join("lib"), Permissions::from_mode(0o755)).unwrap();
        fs::write(usr.join("lib/os-release"), bytes).unwrap();
        fs::set_permissions(usr.join("lib/os-release"), Permissions::from_mode(0o644)).unwrap();
    }
}

fn add_boot_layouts(
    installation: &Installation,
    layout_db: &db::layout::Database,
    package: &package::Id,
    state_id: state::Id,
    head: bool,
    source: &FixtureSchemaSource,
) {
    let mut layouts = Vec::with_capacity(3);
    if head {
        layouts.push(asset(
            installation,
            "lib/systemd/boot/efi/systemd-bootx64.efi",
            b"schema fixture bootloader",
        ));
    }
    if !matches!(source, FixtureSchemaSource::NoBootAssets) {
        layouts.push(asset(
            installation,
            &format!("lib/kernel/6.12.{}/vmlinuz", i32::from(state_id)),
            format!("schema fixture kernel {state_id}").as_bytes(),
        ));
    }
    if let FixtureSchemaSource::OsInfo(bytes) = source {
        layouts.push(asset(installation, "lib/os-info.json", bytes));
    }
    layout_db
        .batch_add(layouts.iter().map(|layout| (package, layout)))
        .unwrap();
}

fn asset(installation: &Installation, path: &str, bytes: &[u8]) -> StonePayloadLayoutRecord {
    let digest = xxhash_rust::xxh3::xxh3_128(bytes);
    if digest != EMPTY_FILE_DIGEST {
        let asset = crate::client::cache::asset_path(installation, &format!("{digest:02x}"));
        fs::create_dir_all(asset.parent().unwrap()).unwrap();
        if !asset.exists() {
            fs::write(&asset, bytes).unwrap();
            fs::set_permissions(&asset, Permissions::from_mode(0o640)).unwrap();
        }
    }
    regular(digest, path)
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

pub(super) fn valid_os_release(id: &str, name: &str) -> Vec<u8> {
    format!("NAME=\"{name}\"\nID=\"{id}\"\nPRETTY_NAME=\"{name} Stable\"\nVERSION_ID=\"1\"\n").into_bytes()
}

pub(super) fn valid_os_info(id: &str, name: &str, former: &[(&str, &str)]) -> Vec<u8> {
    let former = former
        .iter()
        .map(|(id, name)| {
            json!({
                "id": id,
                "name": name,
                "start_date": "2020-01-01T00:00:00Z",
                "end_date": "2021-01-01T00:00:00Z",
                "end_version": "1",
                "announcement": null
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_vec(&json!({
        "os-info-version": "0.1",
        "start_date": "2020-01-01T00:00:00Z",
        "metadata": {
            "identity": {
                "id": id,
                "id_like": "linux",
                "name": name,
                "display": format!("{name} Stable"),
                "ansi_color": null,
                "former_identities": former
            },
            "maintainers": {},
            "version": {
                "full": "1.0.0",
                "short": "1",
                "build_id": "fixture",
                "released": "2026-01-01T00:00:00Z",
                "announcement": null,
                "codename": null
            }
        },
        "system": {
            "composition": { "bases": [], "technology": { "core": [], "optional": [] } },
            "features": {
                "atomic_updates": { "strategy": "atomic", "rollback_support": true },
                "boot": { "bootloader": "systemd-boot", "firmware": { "uefi": true, "secure_boot": false, "bios": false } },
                "filesystem": { "default": "ext4", "supported": ["ext4"] }
            },
            "kernel": { "type": "linux", "name": "linux" },
            "platform": { "architecture": "x86_64", "variant": "generic" },
            "update": {
                "strategy": "atomic",
                "cadence": { "type": "rolling", "sync_interval": null, "sync_day": null, "release_schedule": null, "support_timeline": null },
                "approach": "rolling"
            }
        },
        "resources": { "websites": {}, "social": {}, "funding": {} },
        "security_contact": null
    }))
    .unwrap()
}
