use std::{
    collections::BTreeSet,
    fs::Permissions,
    os::unix::{
        ffi::OsStringExt as _,
        fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink},
        net::UnixListener,
    },
    process::{Command, Stdio},
};

use gluon_config::Source;

use super::*;
use crate::test_support::prepare_private_installation_root;

mod ephemeral_candidate_metadata;
mod external_materialization;
mod fixed_staging_transition;
mod package_request_hardening;
mod root_abi_preflight;
mod self_upgrade_hardening;
mod state_prune;
mod stateful_candidate_metadata;

fn test_installation(root: &Path) -> Installation {
    prepare_private_installation_root(root);
    Installation::open(root, None).unwrap()
}

fn frozen_test_installation(root: &Path) -> Installation {
    prepare_private_installation_root(root);
    Installation::open_frozen(root, None).unwrap()
}

fn test_elf(interpreter: Option<&str>, program_count: usize) -> Vec<u8> {
    assert!(program_count >= 1);
    assert!(interpreter.is_none() || program_count >= 2);
    let class = if usize::BITS == 64 { 2 } else { 1 };
    let little_endian = cfg!(target_endian = "little");
    let header_size = if class == 2 { 64 } else { 52 };
    let program_header_size = if class == 2 { 56 } else { 32 };
    let interpreter_bytes = interpreter.map_or(0, |path| path.len() + 1);
    let interpreter_offset = header_size + program_header_size * program_count;
    let length = interpreter_offset + interpreter_bytes;
    let mut elf = vec![0u8; length];
    elf[..4].copy_from_slice(b"\x7fELF");
    elf[4] = class;
    elf[5] = if little_endian { 1 } else { 2 };
    elf[6] = 1;
    test_elf_write_u16(&mut elf, 16, 2, little_endian);
    test_elf_write_u16(
        &mut elf,
        18,
        native_frozen_elf_machine().expect("tests require a supported Linux ELF architecture"),
        little_endian,
    );
    test_elf_write_u32(&mut elf, 20, 1, little_endian);
    if class == 2 {
        test_elf_write_u64(&mut elf, 24, 0x0040_0000, little_endian);
        test_elf_write_u64(&mut elf, 32, header_size as u64, little_endian);
        test_elf_write_u16(&mut elf, 52, header_size as u16, little_endian);
        test_elf_write_u16(&mut elf, 54, program_header_size as u16, little_endian);
        test_elf_write_u16(&mut elf, 56, program_count as u16, little_endian);

        let load = header_size;
        test_elf_write_u32(&mut elf, load, 1, little_endian);
        test_elf_write_u32(&mut elf, load + 4, 5, little_endian);
        test_elf_write_u64(&mut elf, load + 8, 0, little_endian);
        test_elf_write_u64(&mut elf, load + 16, 0x0040_0000, little_endian);
        test_elf_write_u64(&mut elf, load + 32, length as u64, little_endian);
        test_elf_write_u64(&mut elf, load + 40, length as u64, little_endian);
        test_elf_write_u64(&mut elf, load + 48, 1, little_endian);
        if let Some(interpreter) = interpreter {
            let header = load + program_header_size;
            test_elf_write_u32(&mut elf, header, 3, little_endian);
            test_elf_write_u32(&mut elf, header + 4, 4, little_endian);
            test_elf_write_u64(&mut elf, header + 8, interpreter_offset as u64, little_endian);
            test_elf_write_u64(&mut elf, header + 32, interpreter_bytes as u64, little_endian);
            test_elf_write_u64(&mut elf, header + 40, interpreter_bytes as u64, little_endian);
            test_elf_write_u64(&mut elf, header + 48, 1, little_endian);
            elf[interpreter_offset..interpreter_offset + interpreter.len()].copy_from_slice(interpreter.as_bytes());
        }
    } else {
        test_elf_write_u32(&mut elf, 24, 0x0040_0000, little_endian);
        test_elf_write_u32(&mut elf, 28, header_size as u32, little_endian);
        test_elf_write_u16(&mut elf, 40, header_size as u16, little_endian);
        test_elf_write_u16(&mut elf, 42, program_header_size as u16, little_endian);
        test_elf_write_u16(&mut elf, 44, program_count as u16, little_endian);

        let load = header_size;
        test_elf_write_u32(&mut elf, load, 1, little_endian);
        test_elf_write_u32(&mut elf, load + 4, 0, little_endian);
        test_elf_write_u32(&mut elf, load + 8, 0x0040_0000, little_endian);
        test_elf_write_u32(&mut elf, load + 16, length as u32, little_endian);
        test_elf_write_u32(&mut elf, load + 20, length as u32, little_endian);
        test_elf_write_u32(&mut elf, load + 24, 5, little_endian);
        test_elf_write_u32(&mut elf, load + 28, 1, little_endian);
        if let Some(interpreter) = interpreter {
            let header = load + program_header_size;
            test_elf_write_u32(&mut elf, header, 3, little_endian);
            test_elf_write_u32(&mut elf, header + 4, interpreter_offset as u32, little_endian);
            test_elf_write_u32(&mut elf, header + 16, interpreter_bytes as u32, little_endian);
            test_elf_write_u32(&mut elf, header + 20, interpreter_bytes as u32, little_endian);
            test_elf_write_u32(&mut elf, header + 24, 4, little_endian);
            test_elf_write_u32(&mut elf, header + 28, 1, little_endian);
            elf[interpreter_offset..interpreter_offset + interpreter.len()].copy_from_slice(interpreter.as_bytes());
        }
    }
    elf
}

fn test_elf_write_u16(output: &mut [u8], offset: usize, value: u16, little_endian: bool) {
    let bytes = if little_endian {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    };
    output[offset..offset + bytes.len()].copy_from_slice(&bytes);
}

fn test_elf_write_u32(output: &mut [u8], offset: usize, value: u32, little_endian: bool) {
    let bytes = if little_endian {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    };
    output[offset..offset + bytes.len()].copy_from_slice(&bytes);
}

fn test_elf_write_u64(output: &mut [u8], offset: usize, value: u64, little_endian: bool) {
    let bytes = if little_endian {
        value.to_le_bytes()
    } else {
        value.to_be_bytes()
    };
    output[offset..offset + bytes.len()].copy_from_slice(&bytes);
}

#[allow(clippy::result_large_err)]
fn inspect_test_executable(
    bytes: &[u8],
    binding: &FrozenExecutableBinding,
) -> Result<Option<FrozenExecutableInterpreter>, Error> {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("executable");
    fs::write(&path, bytes).unwrap();
    let file = fs::File::open(path).unwrap();
    let probe_length = bytes.len().min(MAX_FROZEN_SHEBANG_LINE_BYTES + 1);
    inspect_frozen_executable_format(
        &file,
        bytes.len() as u64,
        &bytes[..probe_length],
        Instant::now() + Duration::from_secs(10),
        binding,
    )
}

fn stateful_test_client(root: &Path) -> Client {
    let installation = test_installation(root);
    Client::builder("state-snapshot-test", installation)
        .repositories(repository::Map::default())
        .build()
        .unwrap()
}

fn root_abi_inode(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn assert_root_abi_absent(path: &Path) {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => panic!("failed to inspect expected-absent root ABI path {path:?}: {error}"),
        Ok(metadata) => panic!(
            "expected root ABI path to be absent, found mode {:#o} at {path:?}",
            metadata.mode()
        ),
    }
}

fn assert_root_abi_links(root: &Path) {
    for (source, target) in ROOT_ABI_LINKS {
        assert_eq!(
            fs::read_link(root.join(target)).unwrap().as_os_str().as_bytes(),
            source.as_bytes()
        );
        assert_root_abi_absent(&root.join(format!("{target}.next")));
    }
}

fn generated_system_snapshot(package: &str) -> SystemModel {
    system_model::create(
        repository::Map::default(),
        BTreeSet::from([Provider::package_name(package)]),
    )
}

fn assert_generated_snapshot(path: &Path, expected: &str, package: &str) {
    let encoded = fs::read_to_string(path).unwrap();
    let evaluated =
        system_model::evaluate_snapshot(&Source::new("system-model.glu", encoded.clone())).unwrap();

    assert_eq!(encoded, expected);
    assert_eq!(evaluated.encoded(), encoded);
    assert!(evaluated.packages.contains(&Provider::package_name(package)));
}

fn frozen_publication_destination(parent_path: &Path, name: &str) -> FrozenRootDestination {
    let parent = open_frozen_destination_parent(parent_path).unwrap();
    FrozenRootDestination {
        root_path: parent_path.join(name),
        parent_path: parent_path.to_owned(),
        name: CString::new(name).unwrap(),
        parent_identity: frozen_root_identity(&parent, parent_path).unwrap(),
        parent,
    }
}

fn frozen_publication_fixture(
    parent_path: &Path,
) -> (
    FrozenRootDestination,
    FrozenPrivateDirectory,
    fs::File,
    fs::File,
    Instant,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let destination = frozen_publication_destination(parent_path, "published");
    let stage = create_frozen_private_directory(&destination, b".publication-test-", deadline).unwrap();
    mkdirat(stage.file.as_raw_fd(), "root", Mode::from_bits_truncate(0o755)).unwrap();
    fs::write(stage.path.join("root/candidate"), b"retained candidate").unwrap();
    let root_anchor = openat2_frozen_until(
        stage.file.as_raw_fd(),
        Path::new("root"),
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .unwrap();
    let root = openat2_frozen_until(
        stage.file.as_raw_fd(),
        Path::new("root"),
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_SYMLINKS
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_XDEV) as u64,
        deadline,
    )
    .unwrap();
    assert_eq!(
        frozen_root_identity(&root_anchor, &stage.path.join("root")).unwrap(),
        frozen_root_identity(&root, &stage.path.join("root")).unwrap()
    );
    (destination, stage, root, root_anchor, deadline)
}

fn frozen_discard_fixture(parent_path: &Path) -> (FrozenRootDestination, fs::File, FrozenRootIdentity, Instant) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let destination = frozen_publication_destination(parent_path, "published");
    fs::create_dir(&destination.root_path).unwrap();
    fs::write(destination.root_path.join("candidate"), b"retained candidate").unwrap();
    let pinned =
        open_frozen_named_entry_until(&destination.parent, &destination.name, &destination.root_path, deadline)
            .unwrap()
            .unwrap();
    let identity = frozen_root_identity(&pinned, &destination.root_path).unwrap();
    (destination, pinned, identity, deadline)
}

fn frozen_discard_quarantine_names(parent: &Path) -> Vec<OsString> {
    let mut names = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.as_bytes().starts_with(b".forge-frozen-discard-"))
        .collect::<Vec<_>>();
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    names
}

struct StatefulTransitionFixture {
    _temporary: tempfile::TempDir,
    client: Client,
    previous: State,
    candidate: State,
    previous_snapshot: String,
    candidate_snapshot: String,
}

fn stateful_transition_fixture(archive_candidate: bool) -> StatefulTransitionFixture {
    let temporary = tempfile::tempdir().unwrap();
    let mut client = stateful_test_client(temporary.path());
    let previous = client.state_db.add(&[], Some("previous"), None).unwrap();
    let candidate = client.state_db.add(&[], Some("candidate"), None).unwrap();
    client.installation.active_state = Some(previous.id);

    let previous_model = generated_system_snapshot("previous-package");
    let previous_snapshot = previous_model.encoded().to_owned();
    record_state_id(&client.installation.root, previous.id).unwrap();
    record_system_snapshot(&client.installation.root, previous_model).unwrap();

    let candidate_model = generated_system_snapshot("candidate-package");
    let candidate_snapshot = candidate_model.encoded().to_owned();
    if archive_candidate {
        let candidate_root = client.installation.root_path(candidate.id.to_string());
        record_state_id(&candidate_root, candidate.id).unwrap();
        record_system_snapshot(&candidate_root, candidate_model).unwrap();
    } else {
        // Production fresh-state activation receives an already
        // materialized staging /usr from blit_root. Candidate metadata is
        // deliberately absent until descriptor-bound decoration runs after
        // marker identity preparation.
        record_state_id(&client.installation.staging_dir(), candidate.id).unwrap();
    }

    StatefulTransitionFixture {
        _temporary: temporary,
        client,
        previous,
        candidate,
        previous_snapshot,
        candidate_snapshot,
    }
}

fn injected_state_transition_error(message: &'static str) -> Error {
    Error::Io(io::Error::other(message))
}

fn recovery_tree_token(usr: &Path) -> String {
    let store = crate::tree_marker::TreeMarkerStore::open_path(usr).unwrap();
    store.read_for_recovery().unwrap().token().as_str().to_owned()
}

fn previous_slot_parking_paths(installation: &Installation, state: state::Id) -> Vec<PathBuf> {
    let prefix = format!(".previous-slot-{state}-");
    let mut paths = fs::read_dir(installation.root_path(""))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn archived_candidate_slot_parking_paths(installation: &Installation, state: state::Id) -> Vec<PathBuf> {
    let prefix = format!(".archived-candidate-slot-{state}-");
    let mut paths = fs::read_dir(installation.root_path(""))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn externally_exchange_directory_names(first: &Path, second: &Path) {
    let parking = first.with_extension("external-exchange");
    fs::rename(first, &parking).unwrap();
    fs::rename(second, first).unwrap();
    fs::rename(parking, second).unwrap();
}

fn assert_recovered_stateful_transition(fixture: &StatefulTransitionFixture) {
    let installation = &fixture.client.installation;
    assert_eq!(
        fs::read_to_string(installation.root.join("usr/.stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(&installation.root),
        &fixture.previous_snapshot,
        "previous-package",
    );

    let candidate_root = installation.root_path(fixture.candidate.id.to_string());
    assert_eq!(
        fs::read_to_string(candidate_root.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(&candidate_root),
        &fixture.candidate_snapshot,
        "candidate-package",
    );
    assert!(!installation.staging_path("usr").exists());
    assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
}

fn assert_fresh_candidate_quarantined_and_invalidated(fixture: &StatefulTransitionFixture) {
    let installation = &fixture.client.installation;
    assert_eq!(
        fs::read_to_string(installation.root.join("usr/.stateID")).unwrap(),
        fixture.previous.id.to_string()
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(&installation.root),
        &fixture.previous_snapshot,
        "previous-package",
    );
    assert!(fixture.client.state_db.get(fixture.candidate.id).is_err());
    assert!(!installation.root_path(fixture.candidate.id.to_string()).exists());
    assert!(!installation.root_path(fixture.previous.id.to_string()).exists());
    assert!(!installation.staging_path("usr").exists());

    let quarantines = fs::read_dir(installation.state_quarantine_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(quarantines.len(), 1);
    let quarantine = &quarantines[0];
    assert!(
        quarantine
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&format!("failed-new-state-{}-", fixture.candidate.id))
    );
    assert_eq!(
        fs::read_to_string(quarantine.join("usr/.stateID")).unwrap(),
        fixture.candidate.id.to_string()
    );
    assert_generated_snapshot(
        &system_model::snapshot_path(quarantine),
        &fixture.candidate_snapshot,
        "candidate-package",
    );
}

include!("root_abi_links.rs");
include!("stone_layout_ingestion.rs");
include!("state_metadata.rs");
include!("system_snapshots_and_ephemeral.rs");
include!("frozen_asset_copy.rs");
include!("frozen_executable_limits.rs");
include!("frozen_executable_admission.rs");
include!("frozen_normalization.rs");
include!("frozen_materialization.rs");
include!("frozen_root_lifecycle.rs");
include!("frozen_discard.rs");
include!("frozen_root_validation.rs");
include!("stateful_archived_candidate_recovery.rs");
include!("stateful_previous_tree_recovery.rs");
include!("stateful_quarantine_recovery.rs");
include!("stateful_journal_and_identity_preflight.rs");
include!("stateful_activation_recovery.rs");
