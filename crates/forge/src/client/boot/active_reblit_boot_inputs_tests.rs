use std::{
    cell::Cell,
    collections::BTreeMap,
    os::{
        fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd},
        unix::fs::{FileExt as _, PermissionsExt as _},
    },
    time::{Duration, Instant},
};

use astr::AStr;
use fs_err as fs;
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, fcntl},
    unistd::{Whence, lseek},
};
use stone::{StonePayloadLayoutFile, StonePayloadLayoutRecord};

use super::*;
use crate::{
    Installation, State, client::EMPTY_FILE_DIGEST, db, package, state::Selection,
    test_support::private_installation_tempdir,
};

struct BootInputFixture {
    _temporary: tempfile::TempDir,
    installation: Installation,
    state_db: db::state::Database,
    layout_db: db::layout::Database,
    head: State,
    selected: package::Id,
    bytes_by_digest: BTreeMap<u128, Vec<u8>>,
}

impl BootInputFixture {
    fn new(entries: Vec<(String, Vec<u8>)>) -> Self {
        let temporary = private_installation_tempdir();
        let installation = Installation::open(temporary.path(), None).unwrap();
        let state_db = db::state::Database::new(":memory:").unwrap();
        let layout_db = db::layout::Database::new(":memory:").unwrap();
        let selected = package::Id::from("boot-input-fixture".to_owned());
        let head = state_db
            .add(&[Selection::explicit(selected.clone())], Some("head"), None)
            .unwrap();

        let mut bytes_by_digest = BTreeMap::new();
        let mut layouts = Vec::new();
        for (path, bytes) in entries {
            let digest = xxhash_rust::xxh3::xxh3_128(&bytes);
            if digest != EMPTY_FILE_DIGEST && !bytes_by_digest.contains_key(&digest) {
                write_asset(&installation, digest, &bytes);
            }
            bytes_by_digest.entry(digest).or_insert(bytes);
            layouts.push(regular(digest, &path));
        }
        layout_db
            .batch_add(layouts.iter().map(|layout| (&selected, layout)))
            .unwrap();

        Self {
            _temporary: temporary,
            installation,
            state_db,
            layout_db,
            head,
            selected,
            bytes_by_digest,
        }
    }

    fn prepare(&self) -> Result<ActiveReblitStoneBootInputsOutcome, ActiveReblitStoneBootInputsError> {
        let deadline = Instant::now().checked_add(Duration::from_secs(60)).unwrap();
        PreparedActiveReblitStoneBootInputs::prepare_until(
            &self.installation,
            &self.state_db,
            &self.layout_db,
            &self.head,
            deadline,
        )
    }

    fn prepare_with_policy(
        &self,
        policy: StoneBootInputPolicy,
    ) -> Result<ActiveReblitStoneBootInputsOutcome, ActiveReblitStoneBootInputsError> {
        prepare_with_policy_and_checkpoint(
            &self.installation,
            &self.state_db,
            &self.layout_db,
            &self.head,
            policy,
            |_| {},
        )
    }
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

fn minimal_entries(systemd_boot: &[u8], kernel: &[u8]) -> Vec<(String, Vec<u8>)> {
    vec![
        (
            "lib/systemd/boot/efi/systemd-bootx64.efi".to_owned(),
            systemd_boot.to_vec(),
        ),
        ("lib/kernel/6.12/vmlinuz".to_owned(), kernel.to_vec()),
    ]
}

fn complete_role_entries() -> Vec<(String, Vec<u8>)> {
    vec![
        (
            "lib/systemd/boot/efi/systemd-bootx64.efi".to_owned(),
            b"systemd-boot role bytes".to_vec(),
        ),
        ("lib/os-info.json".to_owned(), b"os-info role bytes".to_vec()),
        (
            "lib/kernel/cmdline.d/00-global.cmdline".to_owned(),
            b"global cmdline role bytes".to_vec(),
        ),
        ("lib/kernel/6.12/vmlinuz".to_owned(), b"kernel role bytes".to_vec()),
        ("lib/kernel/6.12/boot.initrd".to_owned(), b"initrd role bytes".to_vec()),
        (
            "lib/kernel/6.12/kernel.cmdline".to_owned(),
            b"kernel cmdline role bytes".to_vec(),
        ),
    ]
}

fn set_role_limit(policy: &mut StoneBootInputPolicy, role: &BootAssetRole, limit: u64) {
    match role {
        BootAssetRole::SystemdBoot => policy.max_systemd_boot_bytes = limit,
        BootAssetRole::OsInfo => policy.max_os_info_bytes = limit,
        BootAssetRole::GlobalCmdline => policy.max_global_cmdline_bytes = limit,
        BootAssetRole::Kernel { .. } => policy.max_kernel_bytes = limit,
        BootAssetRole::Initrd { .. } => policy.max_initrd_bytes = limit,
        BootAssetRole::KernelCmdline { .. } => policy.max_kernel_cmdline_bytes = limit,
    }
}

fn ready(outcome: ActiveReblitStoneBootInputsOutcome) -> PreparedActiveReblitStoneBootInputs {
    match outcome {
        ActiveReblitStoneBootInputsOutcome::Ready(inputs) => inputs,
        ActiveReblitStoneBootInputsOutcome::NotApplicable(reason) => {
            panic!("expected ready Stone boot inputs, got {reason:?}")
        }
    }
}

fn descriptor_bytes(descriptor: BorrowedFd<'_>, length: u64) -> Vec<u8> {
    let duplicate = fcntl(descriptor.as_raw_fd(), FcntlArg::F_DUPFD_CLOEXEC(0)).unwrap();
    // SAFETY: F_DUPFD_CLOEXEC returned one new descriptor and transfers its
    // ownership exactly once into OwnedFd.
    let duplicate = unsafe { OwnedFd::from_raw_fd(duplicate) };
    let file = std::fs::File::from(duplicate);
    let mut bytes = vec![0; usize::try_from(length).unwrap()];
    file.read_exact_at(&mut bytes, 0).unwrap();
    bytes
}

#[test]
fn ready_binds_each_plan_reference_by_digest_when_plan_and_snapshot_orders_differ() {
    let first = b"first candidate".to_vec();
    let second = b"second candidate".to_vec();
    let mut by_digest = [first, second];
    by_digest.sort_by_key(|bytes| xxhash_rust::xxh3::xxh3_128(bytes));
    let fixture = BootInputFixture::new(minimal_entries(&by_digest[0], &by_digest[1]));

    let inputs = ready(fixture.prepare().unwrap());
    let planned_digests = inputs
        .plan()
        .assets()
        .iter()
        .map(|asset| asset.digest())
        .collect::<Vec<_>>();
    let mut sorted_digests = planned_digests.clone();
    sorted_digests.sort_unstable();
    assert_ne!(
        planned_digests, sorted_digests,
        "fixture must exercise digest lookup, not zip"
    );

    for (planned, bound) in inputs.plan().assets().iter().zip(inputs.assets()) {
        let expected_bytes = &fixture.bytes_by_digest[&bound.digest()];
        assert_eq!(bound.digest(), planned.digest());
        assert_eq!(bound.state_id(), planned.state_id());
        assert_eq!(bound.logical_path(), planned.logical_path());
        assert_eq!(bound.resolved_path(), planned.resolved_path());
        assert_eq!(bound.role(), planned.role());
        assert_eq!(bound.content_identity(), BootContentIdentity::hash(expected_bytes));
        assert_eq!(descriptor_bytes(bound.descriptor(), bound.length()), *expected_bytes);
    }
}

#[test]
fn duplicate_plan_references_share_one_sealed_snapshot() {
    let shared = b"shared critical boot bytes";
    let fixture = BootInputFixture::new(minimal_entries(shared, shared));

    let inputs = ready(fixture.prepare().unwrap());
    let bound = inputs.assets().collect::<Vec<_>>();
    assert_eq!(bound.len(), 2);
    assert_eq!(bound[0].digest(), bound[1].digest());
    assert_eq!(bound[0].content_identity(), bound[1].content_identity());
    assert_eq!(bound[0].content_identity(), BootContentIdentity::hash(shared));
    assert_eq!(bound[0].descriptor().as_raw_fd(), bound[1].descriptor().as_raw_fd());
    assert_eq!(descriptor_bytes(bound[0].descriptor(), bound[0].length()), shared);
    assert_eq!(inputs.referenced_input_bytes(), (shared.len() * 2) as u64);

    let expired = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        inputs.revalidate_until(&fixture.state_db, &fixture.layout_db, expired),
        Err(ActiveReblitStoneBootInputsError::RevalidateProjection(
            ActiveReblitBootProjectionError::DeadlineExceeded { .. }
        ))
    ));
}

#[test]
fn explicit_offset_reads_ignore_a_shared_snapshot_cursor_mutation() {
    let shared = b"shared bytes read independently from an explicit offset";
    let fixture = BootInputFixture::new(minimal_entries(shared, shared));
    let inputs = ready(fixture.prepare().unwrap());
    let bound = inputs.assets().collect::<Vec<_>>();
    assert_eq!(bound[0].descriptor().as_raw_fd(), bound[1].descriptor().as_raw_fd());

    lseek(
        bound[0].descriptor().as_raw_fd(),
        i64::try_from(shared.len()).unwrap(),
        Whence::SeekSet,
    )
    .unwrap();
    assert_eq!(descriptor_bytes(bound[0].descriptor(), bound[0].length()), shared);
    assert_eq!(descriptor_bytes(bound[1].descriptor(), bound[1].length()), shared);
}

#[test]
fn optional_empty_digest_references_share_one_zero_length_binding() {
    let mut entries = minimal_entries(b"systemd boot", b"kernel");
    entries.extend([
        ("lib/kernel/cmdline.d/00-global.cmdline".to_owned(), Vec::new()),
        ("lib/kernel/6.12/kernel.cmdline".to_owned(), Vec::new()),
        ("lib/kernel/6.12/config".to_owned(), Vec::new()),
    ]);
    let fixture = BootInputFixture::new(entries);

    let inputs = ready(fixture.prepare().unwrap());
    let empty = inputs
        .assets()
        .filter(|asset| asset.digest() == EMPTY_FILE_DIGEST)
        .collect::<Vec<_>>();
    assert_eq!(empty.len(), 2);
    assert!(empty.iter().all(|asset| asset.length() == 0));
    assert!(
        empty
            .iter()
            .all(|asset| asset.content_identity() == BootContentIdentity::EMPTY)
    );
    assert!(
        empty
            .windows(2)
            .all(|pair| pair[0].descriptor().as_raw_fd() == pair[1].descriptor().as_raw_fd())
    );
    assert!(descriptor_bytes(empty[0].descriptor(), empty[0].length()).is_empty());
}

#[test]
fn expected_head_mismatch_fails_before_returning_prepared_inputs() {
    let fixture = BootInputFixture::new(minimal_entries(b"systemd boot", b"kernel"));
    let mut expected_head = fixture.head.clone();
    expected_head.summary = Some("not the captured head".to_owned());

    let error = PreparedActiveReblitStoneBootInputs::prepare(
        &fixture.installation,
        &fixture.state_db,
        &fixture.layout_db,
        &expected_head,
    )
    .err()
    .expect("mismatched expected head evidence must fail closed");

    assert!(matches!(
        error,
        ActiveReblitStoneBootInputsError::ExpectedHeadMismatch { .. }
    ));

    let expired = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        PreparedActiveReblitStoneBootInputs::prepare_until(
            &fixture.installation,
            &fixture.state_db,
            &fixture.layout_db,
            &fixture.head,
            expired,
        ),
        Err(ActiveReblitStoneBootInputsError::CaptureProjection(
            ActiveReblitBootProjectionError::DeadlineExceeded { .. }
        ))
    ));
}

#[test]
fn every_role_byte_policy_admits_exact_n_and_rejects_n_plus_one() {
    let fixture = BootInputFixture::new(complete_role_entries());
    let baseline = ready(fixture.prepare().unwrap());
    let roles = baseline
        .assets()
        .map(|asset| (asset.role().clone(), asset.length()))
        .collect::<Vec<_>>();
    assert_eq!(roles.len(), 6);
    drop(baseline);

    for (role, length) in roles {
        assert!(length > 0);
        let mut exact = StoneBootInputPolicy::production();
        set_role_limit(&mut exact, &role, length);
        assert!(matches!(
            fixture.prepare_with_policy(exact).unwrap(),
            ActiveReblitStoneBootInputsOutcome::Ready(_)
        ));

        let mut one_too_many = exact;
        set_role_limit(&mut one_too_many, &role, length - 1);
        match fixture
            .prepare_with_policy(one_too_many)
            .err()
            .expect("one byte above a role limit must fail closed")
        {
            ActiveReblitStoneBootInputsError::RoleByteLimit {
                role: actual_role,
                limit,
                actual,
                ..
            } => {
                assert_eq!(actual_role, role);
                assert_eq!(limit, length - 1);
                assert_eq!(actual, length);
            }
            error => panic!("expected role byte limit for {role:?}, got {error:?}"),
        }
    }
}

#[test]
fn combined_control_byte_policy_admits_exact_n_and_rejects_n_plus_one() {
    let fixture = BootInputFixture::new(complete_role_entries());
    let exact_bytes = ready(fixture.prepare().unwrap()).control_input_bytes();
    assert!(exact_bytes > 0);

    let mut exact = StoneBootInputPolicy::production();
    exact.max_control_input_bytes = exact_bytes;
    assert!(matches!(
        fixture.prepare_with_policy(exact).unwrap(),
        ActiveReblitStoneBootInputsOutcome::Ready(_)
    ));

    let mut one_too_many = exact;
    one_too_many.max_control_input_bytes = exact_bytes - 1;
    assert!(matches!(
        fixture.prepare_with_policy(one_too_many),
        Err(ActiveReblitStoneBootInputsError::ControlInputByteLimit { limit, actual })
            if limit == exact_bytes - 1 && actual == exact_bytes
    ));
}

#[test]
fn referenced_byte_policy_counts_every_reference_to_one_shared_snapshot() {
    let shared = b"one shared snapshot amplified through role references";
    let mut entries = minimal_entries(shared, shared);
    entries.extend((0..5).map(|index| (format!("lib/kernel/6.12/fragment-{index}.initrd"), shared.to_vec())));
    let fixture = BootInputFixture::new(entries);
    let baseline = ready(fixture.prepare().unwrap());
    assert_eq!(baseline.assets().len(), 7);
    let exact_bytes = (shared.len() * 7) as u64;
    assert_eq!(baseline.referenced_input_bytes(), exact_bytes);
    assert_eq!(
        baseline
            .assets()
            .map(|asset| asset.descriptor().as_raw_fd())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        1
    );
    drop(baseline);

    let mut exact = StoneBootInputPolicy::production();
    exact.max_referenced_input_bytes = exact_bytes;
    assert!(matches!(
        fixture.prepare_with_policy(exact).unwrap(),
        ActiveReblitStoneBootInputsOutcome::Ready(_)
    ));

    let mut one_too_many = exact;
    one_too_many.max_referenced_input_bytes = exact_bytes - 1;
    assert!(matches!(
        fixture.prepare_with_policy(one_too_many),
        Err(ActiveReblitStoneBootInputsError::ReferencedInputByteLimit { limit, actual })
            if limit == exact_bytes - 1 && actual == exact_bytes
    ));
}

#[test]
fn binding_work_policy_admits_exact_n_and_rejects_n_plus_one() {
    let fixture = BootInputFixture::new(complete_role_entries());
    let exact_work = ready(fixture.prepare().unwrap()).binding_work();
    assert!(exact_work > 0);

    let mut exact = StoneBootInputPolicy::production();
    exact.max_work = exact_work;
    assert!(matches!(
        fixture.prepare_with_policy(exact).unwrap(),
        ActiveReblitStoneBootInputsOutcome::Ready(_)
    ));

    let mut one_too_many = exact;
    one_too_many.max_work = exact_work - 1;
    assert!(matches!(
        fixture.prepare_with_policy(one_too_many),
        Err(ActiveReblitStoneBootInputsError::BindingWorkLimit { limit, actual })
            if limit == exact_work - 1 && actual == exact_work
    ));

    let terminal_deadline = Instant::now().checked_add(Duration::from_secs(60)).unwrap();
    let expired_terminal = terminal_deadline.checked_add(Duration::from_nanos(1)).unwrap();
    let terminal_checks = Cell::new(0usize);
    let error = prepare_with_policy_until_and_checkpoint(
        &fixture.installation,
        &fixture.state_db,
        &fixture.layout_db,
        &fixture.head,
        StoneBootInputPolicy::production(),
        terminal_deadline,
        |_| {},
        || {
            let check = terminal_checks.get().saturating_add(1);
            terminal_checks.set(check);
            if check == 2 {
                expired_terminal
            } else {
                terminal_deadline
            }
        },
    )
    .err()
    .expect("the complete Stone input must retain a terminal deadline check");
    assert!(matches!(
        error,
        ActiveReblitStoneBootInputsError::DeadlineExceeded { .. }
    ));
    assert_eq!(
        terminal_checks.get(),
        2,
        "the first post-plan check must pass before terminal expiry"
    );
}

#[test]
fn final_revalidation_rejects_state_mutation_after_snapshot_binding() {
    let fixture = BootInputFixture::new(minimal_entries(b"systemd boot", b"kernel"));
    let error = prepare_with_policy_and_checkpoint(
        &fixture.installation,
        &fixture.state_db,
        &fixture.layout_db,
        &fixture.head,
        StoneBootInputPolicy::production(),
        |_| {
            fixture
                .state_db
                .change_summary_for_test(fixture.head.id, Some("mutated before final revalidation"))
                .unwrap();
        },
    )
    .err()
    .expect("state mutation at the final checkpoint must fail closed");

    assert!(matches!(
        error,
        ActiveReblitStoneBootInputsError::RevalidateProjection(ActiveReblitBootProjectionError::StateChanged)
    ));
}

#[test]
fn failed_final_revalidation_drops_every_prepared_snapshot_descriptor() {
    let fixture = BootInputFixture::new(minimal_entries(b"systemd boot", b"kernel"));
    let captured_descriptor = Cell::<Option<RawFd>>::new(None);
    let error = prepare_with_policy_and_checkpoint(
        &fixture.installation,
        &fixture.state_db,
        &fixture.layout_db,
        &fixture.head,
        StoneBootInputPolicy::production(),
        |snapshots| {
            captured_descriptor.set(Some(
                snapshots
                    .snapshots()
                    .next()
                    .expect("prepared plan must own at least one snapshot")
                    .descriptor()
                    .as_raw_fd(),
            ));
            fixture
                .state_db
                .change_summary_for_test(fixture.head.id, Some("force final revalidation failure"))
                .unwrap();
        },
    )
    .err()
    .expect("state mutation at the final checkpoint must fail closed");

    assert!(matches!(
        error,
        ActiveReblitStoneBootInputsError::RevalidateProjection(ActiveReblitBootProjectionError::StateChanged)
    ));
    let descriptor = captured_descriptor
        .get()
        .expect("checkpoint must observe a prepared snapshot descriptor");
    assert_eq!(fcntl(descriptor, FcntlArg::F_GETFD), Err(Errno::EBADF));
}

#[test]
fn final_revalidation_rejects_layout_mutation_after_snapshot_binding() {
    let fixture = BootInputFixture::new(minimal_entries(b"systemd boot", b"kernel"));
    let error = prepare_with_policy_and_checkpoint(
        &fixture.installation,
        &fixture.state_db,
        &fixture.layout_db,
        &fixture.head,
        StoneBootInputPolicy::production(),
        |_| {
            fixture
                .layout_db
                .add(
                    &fixture.selected,
                    &regular(EMPTY_FILE_DIGEST, "share/added-before-final-revalidation"),
                )
                .unwrap();
        },
    )
    .err()
    .expect("layout mutation at the final checkpoint must fail closed");

    assert!(matches!(
        error,
        ActiveReblitStoneBootInputsError::RevalidateProjection(ActiveReblitBootProjectionError::LayoutChanged)
    ));
}
