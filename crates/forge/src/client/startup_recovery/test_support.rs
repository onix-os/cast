use std::{
    fs,
    os::unix::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, PermissionsExt as _, symlink},
    },
    path::{Path, PathBuf},
};

use crate::{
    Installation, State, db,
    state::{self, TransitionId},
    test_support::private_installation_tempdir,
    transition_journal::{
        AbortDisposition, BootId, BootRollback, CandidateRollback, ForwardPhase, MountNamespaceIdentity, Operation,
        Phase, Previous, PreviousOrigin, QuarantineName, RollbackAction, RollbackPlan, RuntimeEpoch,
        RuntimeTreeIdentity, TransitionJournalStore, TransitionRecord, TreeToken, decode,
    },
    tree_marker::TreeMarkerStore,
};

use crate::client::{
    MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal,
    active_state_snapshot::ActiveStateReservation,
    startup_gate::{self, CleanSystemStartup},
    startup_reconciliation::PendingSystemTransition,
};

const OS_RELEASE: &[u8] = b"NAME=Rollback Decision Test\nID=rollback-decision-test\n";
const SYSTEM_MODEL: &[u8] = b"let system = { hostname = \"rollback-decision-test\" } in system\n";
pub(super) const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OperationKind {
    NewState,
    Archived,
    ActiveReblit,
}

impl OperationKind {
    pub(super) const ALL: [Self; 3] = [Self::NewState, Self::Archived, Self::ActiveReblit];

    fn operation(self) -> Operation {
        match self {
            Self::NewState => Operation::NewState,
            Self::Archived => Operation::ActivateArchived,
            Self::ActiveReblit => Operation::ActiveReblit,
        }
    }

    pub(super) fn expected_source_generation(self, phase: Phase) -> u64 {
        match (self, phase) {
            (Self::NewState, Phase::UsrExchangeIntent) => 8,
            (Self::NewState, Phase::UsrExchanged) => 9,
            (Self::NewState, Phase::RootLinksComplete) => 10,
            (Self::Archived, Phase::UsrExchangeIntent) => 4,
            (Self::Archived, Phase::UsrExchanged) => 5,
            (Self::Archived, Phase::RootLinksComplete) => 6,
            (Self::ActiveReblit, Phase::UsrExchangeIntent) => 6,
            (Self::ActiveReblit, Phase::UsrExchanged) => 7,
            (Self::ActiveReblit, Phase::RootLinksComplete) => 8,
            _ => panic!("unsupported rollback-decision source {self:?} at {phase:?}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // the shared rollback-only instantiation does not use IntentPost
pub(super) enum SourceCase {
    IntentPre,
    IntentPost,
    ExchangedPost,
    ExchangedPre,
    RootLinksCompletePost,
    RootLinksCompletePre,
}

impl SourceCase {
    pub(super) fn phase(self) -> Phase {
        match self {
            Self::IntentPre | Self::IntentPost => Phase::UsrExchangeIntent,
            Self::ExchangedPost | Self::ExchangedPre => Phase::UsrExchanged,
            Self::RootLinksCompletePost | Self::RootLinksCompletePre => Phase::RootLinksComplete,
        }
    }

    fn post_exchange(self) -> bool {
        matches!(
            self,
            Self::IntentPost | Self::ExchangedPost | Self::RootLinksCompletePost
        )
    }
}

/// Physical `/usr` layout paired only with a genuine ActiveReblit
/// `BootSyncStarted` forward record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // only the ActiveReblit boot-route test instantiation uses this source
pub(super) enum BootSyncStartedLayout {
    Pre,
    Post,
}

impl BootSyncStartedLayout {
    #[allow(dead_code)] // only the ActiveReblit boot-route test instantiation uses this source
    fn post_exchange(self) -> bool {
        self == Self::Post
    }
}

pub(super) struct Fixture {
    pub(super) _temporary: tempfile::TempDir,
    pub(super) installation: Installation,
    pub(super) database: db::state::Database,
    pub(super) system: MutableSystemCapabilities,
    pub(super) source: TransitionRecord,
    pub(super) kind: OperationKind,
    pub(super) candidate_state: state::Id,
    pub(super) previous_state: state::Id,
    pub(super) active_reblit_reservation: Option<PathBuf>,
}

impl Fixture {
    pub(super) fn new(kind: OperationKind, source: SourceCase) -> Self {
        Self::with_historical_epoch(kind, source, false)
    }

    pub(super) fn historical(kind: OperationKind, source: SourceCase) -> Self {
        Self::with_historical_epoch(kind, source, true)
    }

    fn with_historical_epoch(kind: OperationKind, source: SourceCase, historical: bool) -> Self {
        let fixture = Self::with_forward_source(kind, source.phase(), source.post_exchange(), historical);
        if matches!(
            source,
            SourceCase::ExchangedPost | SourceCase::RootLinksCompletePost | SourceCase::RootLinksCompletePre
        ) {
            install_root_abi(&fixture.installation.root);
        }
        assert_eq!(
            fixture.source.generation,
            kind.expected_source_generation(source.phase()),
            "fixture generation drifted for {kind:?} at {:?}",
            source.phase()
        );
        fixture
    }

    #[allow(dead_code)] // only the ActiveReblit boot-route test instantiation uses this source
    pub(super) fn active_reblit_boot_sync_started(layout: BootSyncStartedLayout, historical: bool) -> Self {
        let fixture = Self::boot_sync_started(OperationKind::ActiveReblit, layout, historical);
        assert_eq!(fixture.source.operation, Operation::ActiveReblit);
        assert_eq!(fixture.source.generation, 11);
        fixture
    }

    #[allow(dead_code)] // focused boot-prefix exclusions also instantiate sibling operations
    pub(super) fn boot_sync_started(kind: OperationKind, layout: BootSyncStartedLayout, historical: bool) -> Self {
        let fixture = Self::with_forward_source(kind, Phase::BootSyncStarted, layout.post_exchange(), historical);
        assert_eq!(fixture.source.phase, Phase::BootSyncStarted);
        fixture
    }

    fn with_forward_source(kind: OperationKind, source_phase: Phase, post_exchange: bool, historical: bool) -> Self {
        let temporary = private_installation_tempdir();
        let root = temporary.path();
        let mut installation = Installation::open(root, None).unwrap();
        let database = db::state::Database::new(":memory:").unwrap();
        let layout_database = db::layout::Database::new(":memory:").unwrap();
        let transition_id = transition_id();
        let (previous_state, candidate_state) = database_states(&database, kind, &transition_id);

        let live_usr = root.join("usr");
        let staging_usr = root.join(".cast/root/staging/usr");
        let (previous_token, previous_runtime) = create_marked_tree(&live_usr, previous_state);
        let (candidate_token, candidate_runtime) = create_marked_tree(&staging_usr, candidate_state);
        if matches!(kind, OperationKind::NewState | OperationKind::ActiveReblit) {
            install_isolation_abi(root);
        }
        let active_reblit_reservation = (kind == OperationKind::ActiveReblit).then(|| {
            let path = root.join(format!(
                ".cast/quarantine/replaced-active-reblit-wrapper-{}-{}-0",
                previous_state,
                previous_token.as_str()
            ));
            create_private_directory(&path);
            path
        });
        if post_exchange {
            exchange_usr_layout(root);
        }
        installation.active_state = Some(if post_exchange { candidate_state } else { previous_state });
        let (creation_epoch, candidate_runtime, previous_runtime) = if historical {
            (
                historical_epoch(),
                RuntimeTreeIdentity {
                    st_dev: 91,
                    inode: 92,
                    mount_id: 93,
                },
                RuntimeTreeIdentity {
                    st_dev: 91,
                    inode: 95,
                    mount_id: 93,
                },
            )
        } else {
            (RuntimeEpoch::capture().unwrap(), candidate_runtime, previous_runtime)
        };
        let preparing = TransitionRecord::preparing(
            transition_id,
            creation_epoch,
            kind.operation(),
            (kind != OperationKind::NewState).then_some(candidate_state.into()),
            candidate_token,
            candidate_runtime,
            Previous {
                id: Some(previous_state.into()),
                tree_token: previous_token,
                usr_runtime_identity: previous_runtime,
                origin: if kind == OperationKind::ActiveReblit {
                    PreviousOrigin::ActiveReblitCorrupt
                } else {
                    PreviousOrigin::ActiveState
                },
            },
            true,
            true,
            QuarantineName::parse("failed-startup-rollback-decision").unwrap(),
        )
        .unwrap();
        let source_record = persist_source_record(&installation, preparing, source_phase, candidate_state);

        let system = MutableSystemCapabilities::from_test_parts(
            &MutableSystemCapabilitiesTestSeal::new(),
            installation.clone(),
            database.clone(),
            layout_database,
        );

        Self {
            _temporary: temporary,
            installation,
            database,
            system,
            source: source_record,
            kind,
            candidate_state,
            previous_state,
            active_reblit_reservation,
        }
    }

    pub(super) fn enter(&self) -> startup_gate::Error {
        let reservation = ActiveStateReservation::acquire().unwrap();
        match CleanSystemStartup::enter(&self.system, &reservation) {
            Ok(_) => panic!("startup unexpectedly admitted an unresolved transition"),
            Err(source) => source,
        }
    }

    pub(super) fn canonical_record(&self) -> TransitionRecord {
        decode(&fs::read(canonical_journal(&self.installation.root)).unwrap()).unwrap()
    }

    pub(super) fn canonical_bytes(&self) -> Vec<u8> {
        fs::read(canonical_journal(&self.installation.root)).unwrap()
    }

    #[allow(dead_code)] // only the root-ABI normalization test instantiation uses this helper
    pub(super) fn set_root_abi_subset(&self, mask: u8) {
        for (index, (name, target)) in ROOT_ABI.into_iter().enumerate() {
            let path = self.installation.root.join(name);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => panic!("remove fixture root ABI link {}: {source}", path.display()),
            }
            if mask & (1 << index) != 0 {
                symlink(target, &path).unwrap();
            }
        }
    }

    #[allow(dead_code)] // only the root-ABI normalization test instantiation uses this helper
    pub(super) fn assert_complete_root_abi(&self) {
        for (name, target) in ROOT_ABI {
            assert_eq!(fs::read_link(self.installation.root.join(name)).unwrap(), Path::new(target));
        }
    }

    pub(super) fn expected_plan(&self) -> RollbackPlan {
        let (source, usr_exchange) = match self.source.phase {
            Phase::UsrExchangeIntent => (ForwardPhase::UsrExchangeIntent, RollbackAction::AlreadySatisfied),
            Phase::UsrExchanged => (ForwardPhase::UsrExchanged, RollbackAction::Pending),
            Phase::RootLinksComplete => (ForwardPhase::RootLinksComplete, RollbackAction::Pending),
            phase => panic!("unsupported rollback-decision plan source {phase:?}"),
        };
        RollbackPlan {
            source,
            previous_archive: RollbackAction::NotRequired,
            usr_exchange,
            candidate: CandidateRollback {
                action: RollbackAction::Pending,
                disposition: if self.kind == OperationKind::Archived {
                    AbortDisposition::Rearchive
                } else {
                    AbortDisposition::Quarantine
                },
            },
            fresh_db: if self.kind == OperationKind::NewState {
                RollbackAction::Pending
            } else {
                RollbackAction::NotRequired
            },
            boot: BootRollback::NotRequired,
            external_effects_may_remain: self.kind != OperationKind::Archived,
        }
    }

    #[allow(dead_code)] // consumed only by the parent-durability test instantiation
    pub(super) fn expected_pending_reverse_plan(&self) -> RollbackPlan {
        assert_eq!(self.source.phase, Phase::UsrExchangeIntent);
        RollbackPlan {
            source: ForwardPhase::UsrExchangeIntent,
            previous_archive: RollbackAction::NotRequired,
            usr_exchange: RollbackAction::Pending,
            candidate: CandidateRollback {
                action: RollbackAction::Pending,
                disposition: if self.kind == OperationKind::Archived {
                    AbortDisposition::Rearchive
                } else {
                    AbortDisposition::Quarantine
                },
            },
            fresh_db: if self.kind == OperationKind::NewState {
                RollbackAction::Pending
            } else {
                RollbackAction::NotRequired
            },
            boot: BootRollback::NotRequired,
            external_effects_may_remain: self.kind != OperationKind::Archived,
        }
    }

    pub(super) fn assert_exact_decision(&self, actual: &TransitionRecord) {
        assert_eq!(actual.phase, Phase::RollbackDecided);
        assert_eq!(actual.generation, self.source.generation + 1);
        assert_eq!(actual.transition_id, self.source.transition_id);
        assert_eq!(actual.operation, self.source.operation);
        assert_eq!(actual.creation_epoch, self.source.creation_epoch);
        assert_eq!(actual.candidate, self.source.candidate);
        assert_eq!(actual.previous, self.source.previous);
        assert_eq!(actual.options, self.source.options);
        assert_eq!(actual.quarantine_name, self.source.quarantine_name);
        assert_eq!(actual.rollback, Some(self.expected_plan()));
    }

    #[allow(dead_code)] // consumed only by the parent-durability test instantiation
    pub(super) fn assert_exact_pending_reverse_decision(&self, actual: &TransitionRecord) {
        assert_eq!(actual.phase, Phase::RollbackDecided);
        assert_eq!(actual.generation, self.source.generation + 1);
        assert_eq!(actual.transition_id, self.source.transition_id);
        assert_eq!(actual.operation, self.source.operation);
        assert_eq!(actual.creation_epoch, self.source.creation_epoch);
        assert_eq!(actual.candidate, self.source.candidate);
        assert_eq!(actual.previous, self.source.previous);
        assert_eq!(actual.options, self.source.options);
        assert_eq!(actual.quarantine_name, self.source.quarantine_name);
        assert_eq!(actual.rollback, Some(self.expected_pending_reverse_plan()));
    }

    pub(super) fn database_snapshot(&self) -> DatabaseSnapshot {
        DatabaseSnapshot {
            states: self.database.all().unwrap(),
            in_flight: self.database.audit_in_flight_transition().unwrap(),
            candidate_ownership: self
                .database
                .transition_ownership(self.candidate_state, &self.source.transition_id)
                .unwrap(),
            candidate_provenance: self.database.metadata_provenance(self.candidate_state).unwrap(),
            previous_ownership: self
                .database
                .transition_ownership(self.previous_state, &self.source.transition_id)
                .unwrap(),
            previous_provenance: self.database.metadata_provenance(self.previous_state).unwrap(),
        }
    }

    pub(super) fn namespace_snapshot(&self) -> Vec<NamespaceEntry> {
        let mut entries = Vec::new();
        snapshot_directory(&self.installation.root, &self.installation.root, &mut entries);
        entries
    }

    #[allow(dead_code)] // consumed only by the parent-durability test instantiation
    pub(super) fn durability_parent_identities(&self) -> ((u64, u64), (u64, u64)) {
        let staging = fs::symlink_metadata(self.installation.root.join(".cast/root/staging")).unwrap();
        let root = self.installation.root_directory().metadata().unwrap();
        ((staging.dev(), staging.ino()), (root.dev(), root.ino()))
    }

    pub(super) fn assert_source_unchanged(&self) {
        assert_eq!(self.canonical_record(), self.source);
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct DatabaseSnapshot {
    states: Vec<State>,
    in_flight: Option<db::state::InFlightTransition>,
    candidate_ownership: db::state::TransitionOwnership,
    candidate_provenance: Option<db::state::MetadataProvenance>,
    previous_ownership: db::state::TransitionOwnership,
    previous_provenance: Option<db::state::MetadataProvenance>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct NamespaceEntry {
    relative: PathBuf,
    kind: NamespaceEntryKind,
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NamespaceEntryKind {
    Directory,
    File,
    Symlink,
}

pub(super) fn pending(error: &startup_gate::Error) -> &PendingSystemTransition {
    match error {
        startup_gate::Error::RecoveryPending(pending) => pending,
        other => panic!("expected recovery-pending startup result, got {other:?}"),
    }
}

pub(super) fn create_private_directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

pub(super) fn canonical_journal(root: &Path) -> PathBuf {
    root.join(".cast/journal/state-transition")
}

fn transition_id() -> TransitionId {
    TransitionId::parse("0123456789abcdef0123456789abcdef").unwrap()
}

fn historical_epoch() -> RuntimeEpoch {
    RuntimeEpoch {
        boot_id: BootId::parse("fedcba98-7654-4abc-8def-0123456789ab").unwrap(),
        mount_namespace: MountNamespaceIdentity { st_dev: 81, inode: 82 },
    }
}

fn database_states(
    database: &db::state::Database,
    kind: OperationKind,
    transition_id: &TransitionId,
) -> (state::Id, state::Id) {
    match kind {
        OperationKind::NewState => {
            let previous = database.add(&[], Some("rollback previous"), None).unwrap().id;
            let candidate = add_state_with_provenance(database, transition_id, "rollback fresh candidate", false);
            (previous, candidate)
        }
        OperationKind::Archived => {
            let previous = database.add(&[], Some("rollback previous"), None).unwrap().id;
            let candidate_transition = TransitionId::parse("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").unwrap();
            let candidate =
                add_state_with_provenance(database, &candidate_transition, "rollback archived candidate", true);
            (previous, candidate)
        }
        OperationKind::ActiveReblit => {
            let candidate_transition = TransitionId::parse("dddddddddddddddddddddddddddddddd").unwrap();
            let state = add_state_with_provenance(database, &candidate_transition, "rollback active reblit", true);
            (state, state)
        }
    }
}

fn add_state_with_provenance(
    database: &db::state::Database,
    transition: &TransitionId,
    summary: &str,
    clear_transition: bool,
) -> state::Id {
    let state = database
        .add_with_transition(transition, &[], Some(summary), None)
        .unwrap();
    let provenance = db::state::MetadataProvenance::from_outputs(OS_RELEASE, SYSTEM_MODEL);
    database
        .insert_fresh_metadata_provenance_if_transition_matches(state.id, transition, &provenance)
        .unwrap();
    if clear_transition {
        database.clear_transition_if_matches(state.id, transition).unwrap();
    }
    state.id
}

fn create_marked_tree(path: &Path, state: state::Id) -> (TreeToken, RuntimeTreeIdentity) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    let state_id = path.join(".stateID");
    fs::write(&state_id, state.to_string().as_bytes()).unwrap();
    fs::set_permissions(&state_id, fs::Permissions::from_mode(0o644)).unwrap();
    let store = TreeMarkerStore::open_path(path).unwrap();
    let marker = store.adopt_or_create_before_journal().unwrap();
    let token = marker.token().clone();
    let runtime = RuntimeTreeIdentity::capture_directory(store.retained_directory()).unwrap();
    (token, runtime)
}

fn install_isolation_abi(root: &Path) {
    for (name, target) in ROOT_ABI {
        symlink(target, root.join(".cast/root/isolation").join(name)).unwrap();
    }
}

pub(super) fn install_root_abi(root: &Path) {
    for (name, target) in ROOT_ABI {
        symlink(target, root.join(name)).unwrap();
    }
}

pub(super) fn exchange_usr_layout(root: &Path) {
    let live = root.join("usr");
    let staging = root.join(".cast/root/staging/usr");
    let parked = root.join(".cast/root/.exchange-fixture-previous");
    fs::rename(&live, &parked).unwrap();
    fs::rename(&staging, &live).unwrap();
    fs::rename(&parked, &staging).unwrap();
}

fn persist_source_record(
    installation: &Installation,
    mut record: TransitionRecord,
    target: Phase,
    candidate: state::Id,
) -> TransitionRecord {
    let store = TransitionJournalStore::open_retained(installation.root_directory(), &installation.root).unwrap();
    store.create(&record).unwrap();
    while record.phase != target {
        let allocated = (record.phase == Phase::FreshStateAllocating).then_some(candidate.into());
        let next = record.forward_successor(allocated).unwrap();
        store.advance(&record, &next).unwrap();
        record = next;
    }
    drop(store);
    record
}

fn snapshot_directory(root: &Path, directory: &Path, output: &mut Vec<NamespaceEntry>) {
    let mut children = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .as_bytes()
            .cmp(right.file_name().unwrap_or_default().as_bytes())
    });
    for path in children {
        let relative = path.strip_prefix(root).unwrap().to_owned();
        if relative.starts_with(Path::new(".cast/journal")) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).unwrap();
        let file_type = metadata.file_type();
        let (kind, payload) = if file_type.is_dir() {
            (NamespaceEntryKind::Directory, Vec::new())
        } else if file_type.is_file() {
            (NamespaceEntryKind::File, fs::read(&path).unwrap())
        } else if file_type.is_symlink() {
            (
                NamespaceEntryKind::Symlink,
                fs::read_link(&path).unwrap().as_os_str().as_bytes().to_vec(),
            )
        } else {
            panic!("unexpected fixture namespace entry kind at {}", path.display());
        };
        output.push(NamespaceEntry {
            relative,
            kind,
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            payload,
        });
        if file_type.is_dir() {
            snapshot_directory(root, &path, output);
        }
    }
}
