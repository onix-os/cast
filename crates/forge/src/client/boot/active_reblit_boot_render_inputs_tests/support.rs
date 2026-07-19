use std::{
    fs::{self, File, Permissions},
    os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use astr::AStr;
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use super::*;
use crate::{
    State,
    client::{
        EMPTY_FILE_DIGEST, active_reblit_boot_inputs::ActiveReblitStoneBootInputsOutcome,
        active_reblit_local_boot_policy::PreparedActiveReblitLocalBootPolicy,
        active_reblit_root_filesystem_intent::PreparedActiveReblitRootFilesystemIntent,
    },
    package,
    state::Selection,
    test_support::private_installation_tempdir,
    transition_identity::PreparedActiveReblitBootStateRoots,
    tree_marker::TreeMarkerStore,
};

pub(super) const ROOT_LOCATOR: &str = "PARTUUID=11111111-2222-3333-4444-555555555555";

#[derive(Clone)]
pub(super) struct KernelSpec {
    pub(super) version: String,
    pub(super) initrds: Vec<(String, Vec<u8>)>,
}

#[derive(Clone)]
pub(super) struct StateSpec {
    pub(super) kernels: Vec<KernelSpec>,
    pub(super) cmdlines: Vec<(String, Vec<u8>)>,
}

pub(super) struct RenderFixture {
    _temporary: tempfile::TempDir,
    pub(super) installation: Installation,
    pub(super) state_db: db::state::Database,
    pub(super) layout_db: db::layout::Database,
    pub(super) head: State,
    pub(super) histories: Vec<State>,
    pub(super) head_usr: File,
    head_package: package::Id,
}

impl KernelSpec {
    pub(super) fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            initrds: Vec::new(),
        }
    }

    pub(super) fn with_initrd(mut self, name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        self.initrds.push((name.into(), bytes.into()));
        self
    }
}

impl StateSpec {
    pub(super) fn one_kernel(version: impl Into<String>) -> Self {
        Self {
            kernels: vec![KernelSpec::new(version)],
            cmdlines: Vec::new(),
        }
    }

    pub(super) fn with_kernel(mut self, kernel: KernelSpec) -> Self {
        self.kernels.push(kernel);
        self
    }

    pub(super) fn with_cmdline(mut self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        self.cmdlines.push((path.into(), bytes.into()));
        self
    }
}

impl RenderFixture {
    pub(super) fn new(head_spec: StateSpec, history_specs: Vec<StateSpec>) -> Self {
        let temporary = private_installation_tempdir();
        let state_db = db::state::Database::new(":memory:").unwrap();
        let layout_db = db::layout::Database::new(":memory:").unwrap();
        let head_package = package::Id::from("render-head".to_owned());
        let head = state_db
            .add(&[Selection::explicit(head_package.clone())], Some("render head"), None)
            .unwrap();
        let mut histories = Vec::new();
        let mut history_packages = Vec::new();
        for index in 0..history_specs.len() {
            let package = package::Id::from(format!("render-history-{index}"));
            histories.push(
                state_db
                    .add(
                        &[Selection::explicit(package.clone())],
                        Some(&format!("render history {index}")),
                        None,
                    )
                    .unwrap(),
            );
            history_packages.push(package);
        }

        create_exact_tree(&temporary.path().join("usr"), head.id);
        let installation = Installation::open(temporary.path(), None).unwrap();
        install_generated_schema(&installation.root.join("usr"), "head", "Render Head");
        install_root_intent(&installation);
        let head_usr = File::open(installation.root.join("usr")).unwrap();
        add_state_layouts(&installation, &layout_db, &head_package, &head_spec, true);

        for (index, ((history, package), spec)) in
            histories.iter().zip(&history_packages).zip(&history_specs).enumerate()
        {
            let wrapper = installation.root_path(history.id.to_string());
            fs::create_dir(&wrapper).unwrap();
            fs::set_permissions(&wrapper, Permissions::from_mode(0o700)).unwrap();
            create_exact_tree(&wrapper.join("usr"), history.id);
            install_generated_schema(
                &wrapper.join("usr"),
                &format!("history{index}"),
                &format!("Render History {index}"),
            );
            add_state_layouts(&installation, &layout_db, package, spec, false);
        }

        Self {
            _temporary: temporary,
            installation,
            state_db,
            layout_db,
            head,
            histories,
            head_usr,
            head_package,
        }
    }

    pub(super) fn stone(&self) -> PreparedActiveReblitStoneBootInputs {
        match PreparedActiveReblitStoneBootInputs::prepare_until(
            &self.installation,
            &self.state_db,
            &self.layout_db,
            &self.head,
            future_deadline(),
        )
        .unwrap()
        {
            ActiveReblitStoneBootInputsOutcome::Ready(stone) => stone,
            ActiveReblitStoneBootInputsOutcome::NotApplicable(reason) => {
                panic!("render fixture must be bootable: {reason:?}")
            }
        }
    }

    pub(super) fn roots(&self, stone: &PreparedActiveReblitStoneBootInputs) -> PreparedActiveReblitBootStateRoots {
        PreparedActiveReblitBootStateRoots::prepare_until(
            &self.installation,
            &self.head_usr,
            self.head.id,
            stone.state_ids(),
            future_deadline(),
        )
        .unwrap()
    }

    pub(super) fn local_policy(&self) -> PreparedActiveReblitLocalBootPolicy {
        PreparedActiveReblitLocalBootPolicy::prepare_until(&self.installation, future_deadline()).unwrap()
    }

    pub(super) fn root_intent(&self) -> PreparedActiveReblitRootFilesystemIntent {
        PreparedActiveReblitRootFilesystemIntent::prepare_until(&self.installation, future_deadline()).unwrap()
    }

    pub(super) fn policy_directory(&self) -> PathBuf {
        let path = self.installation.root.join("etc/kernel/cmdline.d");
        fs::create_dir_all(&path).unwrap();
        for directory in [
            self.installation.root.join("etc"),
            self.installation.root.join("etc/kernel"),
            path.clone(),
        ] {
            fs::set_permissions(directory, Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    pub(super) fn write_local(&self, name: &str, bytes: impl AsRef<[u8]>) {
        let path = self.policy_directory().join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(path, Permissions::from_mode(0o644)).unwrap();
    }

    pub(super) fn mask_local(&self, name: &str) {
        std::os::unix::fs::symlink("/dev/null", self.policy_directory().join(name)).unwrap();
    }

    pub(super) fn exclude_history(&self, index: usize) {
        fs::remove_dir_all(self.installation.root_path(self.histories[index].id.to_string())).unwrap();
    }

    pub(super) fn add_irrelevant_head_layout(&self) {
        let layout = regular(EMPTY_FILE_DIGEST, "share/render-race-marker");
        self.layout_db.batch_add([(&self.head_package, &layout)]).unwrap();
    }
}

pub(super) fn prepare_static<'stone, 'roots>(
    fixture: &RenderFixture,
    stone: &'stone PreparedActiveReblitStoneBootInputs,
    roots: &'roots PreparedActiveReblitBootStateRoots,
) -> PreparedActiveReblitBootRenderInputs<'stone, 'roots> {
    PreparedActiveReblitBootRenderInputs::prepare_until(stone, roots, &fixture.installation, future_deadline()).unwrap()
}

pub(super) fn future_deadline() -> Instant {
    Instant::now().checked_add(Duration::from_secs(60)).unwrap()
}

pub(super) fn expired_deadline() -> Instant {
    Instant::now().checked_sub(Duration::from_secs(1)).unwrap()
}

pub(super) fn simple_fixture() -> RenderFixture {
    RenderFixture::new(StateSpec::one_kernel("6.12"), Vec::new())
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct TreeSnapshot(Vec<TreeEntry>);

#[derive(Debug, Eq, PartialEq)]
struct TreeEntry {
    relative: PathBuf,
    mode: u32,
    links: u64,
    bytes: Vec<u8>,
}

impl TreeSnapshot {
    pub(super) fn capture(root: &Path) -> Self {
        let mut entries = Vec::new();
        capture_directory(root, root, &mut entries);
        Self(entries)
    }
}

fn capture_directory(root: &Path, directory: &Path, entries: &mut Vec<TreeEntry>) {
    let mut children = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort();
    for path in children {
        let metadata = fs::symlink_metadata(&path).unwrap();
        let bytes = if metadata.file_type().is_file() {
            fs::read(&path).unwrap()
        } else if metadata.file_type().is_symlink() {
            fs::read_link(&path).unwrap().as_os_str().as_bytes().to_vec()
        } else {
            Vec::new()
        };
        entries.push(TreeEntry {
            relative: path.strip_prefix(root).unwrap().to_owned(),
            mode: metadata.permissions().mode(),
            links: std::os::unix::fs::MetadataExt::nlink(&metadata),
            bytes,
        });
        if metadata.file_type().is_dir() {
            capture_directory(root, &path, entries);
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

fn install_generated_schema(usr: &Path, id: &str, name: &str) {
    fs::create_dir_all(usr.join("lib")).unwrap();
    fs::set_permissions(usr.join("lib"), Permissions::from_mode(0o755)).unwrap();
    let bytes = format!("NAME=\"{name}\"\nID=\"{id}\"\nPRETTY_NAME=\"{name}\"\nVERSION_ID=\"1\"\n");
    fs::write(usr.join("lib/os-release"), bytes).unwrap();
    fs::set_permissions(usr.join("lib/os-release"), Permissions::from_mode(0o644)).unwrap();
}

fn install_root_intent(installation: &Installation) {
    let directory = installation.root.join("etc/cast");
    fs::create_dir_all(&directory).unwrap();
    for path in [installation.root.join("etc"), directory] {
        fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
    }
    let source =
        format!("let cast = import! cast.root_filesystem.v1\ncast.root_filesystem {{ root = {ROOT_LOCATOR:?} }}\n");
    let path = installation.root.join("etc/cast/root-filesystem.glu");
    fs::write(&path, source).unwrap();
    fs::set_permissions(path, Permissions::from_mode(0o644)).unwrap();
}

fn add_state_layouts(
    installation: &Installation,
    layout_db: &db::layout::Database,
    package: &package::Id,
    spec: &StateSpec,
    head: bool,
) {
    let mut layouts = Vec::new();
    if head {
        layouts.push(asset(
            installation,
            "lib/systemd/boot/efi/systemd-bootx64.efi",
            b"render fixture systemd bootloader",
        ));
    }
    for kernel in &spec.kernels {
        layouts.push(asset(
            installation,
            &format!("lib/kernel/{}/vmlinuz", kernel.version),
            format!("render kernel {}", kernel.version).as_bytes(),
        ));
        for (name, bytes) in &kernel.initrds {
            layouts.push(asset(
                installation,
                &format!("lib/kernel/{}/{}", kernel.version, name),
                bytes,
            ));
        }
    }
    layouts.extend(
        spec.cmdlines
            .iter()
            .map(|(path, bytes)| asset(installation, path, bytes)),
    );
    layout_db
        .batch_add(layouts.iter().map(|layout| (package, layout)))
        .unwrap();
}

fn asset(installation: &Installation, path: &str, bytes: &[u8]) -> StonePayloadLayoutRecord {
    let digest = xxhash_rust::xxh3::xxh3_128(bytes);
    if digest != EMPTY_FILE_DIGEST {
        let target = crate::client::cache::asset_path(installation, &format!("{digest:02x}"));
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        if !target.exists() {
            fs::write(&target, bytes).unwrap();
            fs::set_permissions(target, Permissions::from_mode(0o640)).unwrap();
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
