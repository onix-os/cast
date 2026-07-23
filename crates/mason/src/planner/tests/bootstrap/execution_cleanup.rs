#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectoryWitness {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl DirectoryWitness {
    fn capture(fixture: &str, role: &str, path: &Path) -> Self {
        let metadata = fs::symlink_metadata(path)
            .unwrap_or_else(|error| panic!("{fixture}: inspect {role} at {path:?}: {error}"));
        assert!(metadata.file_type().is_dir(), "{fixture}: {role} is not a directory");
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

const MAX_ASSET_INVENTORY_ENTRIES: usize = 1_000_000;
const MAX_ASSET_INVENTORY_DEPTH: usize = 128;
const MAX_ASSET_INVENTORY_PATH_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetEntryKind {
    Directory,
    RegularFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AssetEntryWitness {
    kind: AssetEntryKind,
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl AssetEntryWitness {
    fn capture(fixture: &str, path: &Path, expected_device: u64) -> Self {
        let metadata = fs::symlink_metadata(path)
            .unwrap_or_else(|error| panic!("{fixture}: inspect shared Forge asset entry {path:?}: {error}"));
        let kind = if metadata.file_type().is_dir() {
            AssetEntryKind::Directory
        } else if metadata.file_type().is_file() {
            AssetEntryKind::RegularFile
        } else {
            panic!("{fixture}: shared Forge asset cache contains a symlink or special entry at {path:?}")
        };
        assert_eq!(
            metadata.dev(),
            expected_device,
            "{fixture}: shared Forge asset cache crosses a mount at {path:?}"
        );
        assert!(metadata.nlink() >= 1, "{fixture}: shared Forge asset entry has no links");
        Self {
            kind,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
            links: metadata.nlink(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct AssetInventory {
    entries: BTreeMap<PathBuf, AssetEntryWitness>,
}

impl AssetInventory {
    pub(super) fn capture(fixture: &str, root: &Path) -> Self {
        let root_metadata = fs::symlink_metadata(root)
            .unwrap_or_else(|error| panic!("{fixture}: inspect shared Forge asset-cache root {root:?}: {error}"));
        assert!(
            root_metadata.file_type().is_dir(),
            "{fixture}: shared Forge asset-cache root is not a directory"
        );
        let root_device = root_metadata.dev();
        let root_witness = AssetEntryWitness::capture(fixture, root, root_device);
        let mut entries = BTreeMap::new();
        assert!(entries.insert(PathBuf::new(), root_witness).is_none());
        let mut pending = vec![(PathBuf::new(), 0usize)];
        let mut aggregate_path_bytes = 0usize;

        while let Some((relative, depth)) = pending.pop() {
            let directory = root.join(&relative);
            let mut children = Vec::new();
            for entry in fs::read_dir(&directory)
                .unwrap_or_else(|error| panic!("{fixture}: read shared Forge asset directory {directory:?}: {error}"))
            {
                assert!(
                    entries.len() + children.len() < MAX_ASSET_INVENTORY_ENTRIES,
                    "{fixture}: shared Forge asset cache exceeds its {MAX_ASSET_INVENTORY_ENTRIES}-entry boundary"
                );
                children.push(entry.unwrap_or_else(|error| {
                    panic!("{fixture}: enumerate shared Forge asset directory {directory:?}: {error}")
                }));
            }
            children.sort_by_key(|entry| entry.file_name());

            for entry in children.into_iter().rev() {
                let child_depth = depth
                    .checked_add(1)
                    .expect("shared Forge asset inventory depth overflowed");
                assert!(
                    child_depth <= MAX_ASSET_INVENTORY_DEPTH,
                    "{fixture}: shared Forge asset cache exceeds its {MAX_ASSET_INVENTORY_DEPTH}-component boundary"
                );
                let child = relative.join(entry.file_name());
                let path_bytes = std::os::unix::ffi::OsStrExt::as_bytes(child.as_os_str()).len();
                aggregate_path_bytes = aggregate_path_bytes
                    .checked_add(path_bytes)
                    .expect("shared Forge asset inventory path-byte count overflowed");
                assert!(
                    aggregate_path_bytes <= MAX_ASSET_INVENTORY_PATH_BYTES,
                    "{fixture}: shared Forge asset cache exceeds its {MAX_ASSET_INVENTORY_PATH_BYTES}-byte path boundary"
                );
                let witness = AssetEntryWitness::capture(fixture, &root.join(&child), root_device);
                assert!(
                    entries.insert(child.clone(), witness).is_none(),
                    "{fixture}: shared Forge asset inventory repeated {child:?}"
                );
                if witness.kind == AssetEntryKind::Directory {
                    pending.push((child, child_depth));
                }
            }
        }

        assert!(
            entries
                .values()
                .any(|entry| entry.kind == AssetEntryKind::RegularFile),
            "{fixture}: shared Forge asset cache contains no regular asset"
        );
        assert_eq!(
            AssetEntryWitness::capture(fixture, root, root_device),
            root_witness,
            "{fixture}: shared Forge asset-cache root changed while inventorying it"
        );
        Self { entries }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SharedForgeAssetWitness {
    root: PathBuf,
    inventory: AssetInventory,
}

impl SharedForgeAssetWitness {
    fn capture(fixture: &str, forge_dir: &Path) -> Self {
        let root = forge_dir.join(".cast/assets/v2");
        let inventory = AssetInventory::capture(fixture, &root);
        Self { root, inventory }
    }

    fn assert_retained(&self, fixture: &str) {
        assert_eq!(
            AssetInventory::capture(fixture, &self.root),
            self.inventory,
            "{fixture}: complete shared Forge asset inventory changed during derivation cleanup"
        );
    }
}

struct ExecutionCleanupWitness {
    root: PathBuf,
    build: PathBuf,
    artefacts: PathBuf,
    published: PathBuf,
    published_identity: DirectoryWitness,
    shared_assets: SharedForgeAssetWitness,
}

impl ExecutionCleanupWitness {
    fn capture(fixture: &str, planned: &Planned, published: &Path, forge_dir: &Path) -> Self {
        let root = planned.runtime.paths.rootfs().host;
        let build = planned.runtime.paths.build().host;
        let artefacts = planned.runtime.paths.artefacts().host;
        DirectoryWitness::capture(fixture, "live frozen root", &root);
        DirectoryWitness::capture(fixture, "live build scratch", &build);
        DirectoryWitness::capture(fixture, "live artifact scratch", &artefacts);
        let published_identity = DirectoryWitness::capture(fixture, "published derivation bundle", published);
        assert_live_derivation_capacity(fixture, planned, 1);
        Self {
            root,
            build,
            artefacts,
            published: published.to_owned(),
            published_identity,
            shared_assets: SharedForgeAssetWitness::capture(fixture, forge_dir),
        }
    }

    fn assert_cleaned(
        self,
        fixture: &str,
        planned: &Planned,
        expected_bundle: &BTreeMap<String, Vec<u8>>,
    ) {
        for (role, path) in [
            ("frozen root", &self.root),
            ("build scratch", &self.build),
            ("artifact scratch", &self.artefacts),
        ] {
            assert_entry_absent(fixture, role, path);
        }
        assert_eq!(
            DirectoryWitness::capture(
                fixture,
                "published derivation bundle after runtime cleanup",
                &self.published,
            ),
            self.published_identity,
            "{fixture}: published derivation root identity changed during runtime cleanup"
        );
        let retained = bundle::assert_fixture_bundle(
            fixture,
            planned,
            &self.published,
            bundle::BundleRootRole::Published,
        );
        assert_eq!(
            &retained, expected_bundle,
            "{fixture}: published bundle bytes changed during runtime cleanup"
        );
        self.shared_assets.assert_retained(fixture);
        assert_live_derivation_capacity(fixture, planned, 0);
        assert_no_success_cleanup_quarantine(fixture, planned);
    }
}

fn assert_live_derivation_capacity(fixture: &str, planned: &Planned, expected: usize) {
    for (role, path) in [
        ("root", planned.runtime.paths.rootfs().host),
        ("build", planned.runtime.paths.build().host),
        ("artifact", planned.runtime.paths.artefacts().host),
    ] {
        let parent = path.parent().expect("derivation workspace path has no parent");
        let count = fs::read_dir(parent)
            .unwrap_or_else(|error| panic!("{fixture}: read {role} workspace parent {parent:?}: {error}"))
            .map(|entry| entry.unwrap())
            .filter(|entry| {
                let name = entry.file_name();
                let bytes = std::os::unix::ffi::OsStrExt::as_bytes(name.as_os_str());
                bytes.len() == 64 && bytes.iter().all(u8::is_ascii_hexdigit)
            })
            .count();
        assert_eq!(
            count, expected,
            "{fixture}: live derivation {role} capacity must be exactly {expected}, found {count}"
        );
    }
}

fn assert_no_success_cleanup_quarantine(fixture: &str, planned: &Planned) {
    let derivation = planned.plan.derivation_id();
    for path in [
        planned.runtime.paths.build().host,
        planned.runtime.paths.artefacts().host,
    ] {
        let quarantine = path
            .parent()
            .expect("derivation scratch has no parent")
            .join(format!(".{}.cast-stale", derivation.as_str()));
        assert_entry_absent(fixture, "successful scratch cleanup quarantine", &quarantine);
    }

    let root_parent = planned
        .runtime
        .paths
        .rootfs()
        .host
        .parent()
        .expect("derivation root has no parent")
        .to_owned();
    let quarantine_count = fs::read_dir(&root_parent)
        .unwrap_or_else(|error| panic!("{fixture}: read frozen-root parent {root_parent:?}: {error}"))
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| std::os::unix::ffi::OsStrExt::as_bytes(name.as_os_str()).starts_with(b".forge-frozen-discard-"))
        .count();
    assert_eq!(
        quarantine_count, 0,
        "{fixture}: successful frozen-root cleanup left a Forge quarantine"
    );
}

fn assert_entry_absent(fixture: &str, role: &str, path: &Path) {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => panic!("{fixture}: inspect absent {role} at {path:?}: {error}"),
        Ok(_) => panic!("{fixture}: {role} remains after cleanup at {path:?}"),
    }
}
