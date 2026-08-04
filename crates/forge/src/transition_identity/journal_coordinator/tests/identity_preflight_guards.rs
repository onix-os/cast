// Ports of the identity-preparation guards from the legacy client route.
//
// These fire inside the shared `prepare_candidate` before any marker is
// published, so they are driven through `StatefulTreeIdentity` directly rather
// than through a coordinator. The legacy tests reached the same code through
// `apply_stateful_blit_with_checkpoint` and asserted the client's
// `StatefulTreeIdentityPreparationFailed` wrapper; that wrapper is legacy-route
// detail, but the guard inside it is shared and outlives the route.
//
// `fixture_parts` is deliberately not used here: it prepares the identity
// internally and unwraps it, so there is no seam to plant a precondition
// *before* preparation runs. These build the installation inline instead.

struct PreflightFixture {
    _temporary: tempfile::TempDir,
    installation: Installation,
    database: db::state::Database,
    previous_state: state::Id,
    candidate_state: state::Id,
    candidate_path: PathBuf,
}

impl PreflightFixture {
    fn live_marker(&self) -> PathBuf {
        self.installation.root.join("usr/.cast-tree-id")
    }

    fn candidate_marker(&self) -> PathBuf {
        self.candidate_path.join(".cast-tree-id")
    }

    fn canonical_journal(&self) -> PathBuf {
        self.installation.root.join(".cast/journal/state-transition")
    }

    fn prepare(&self) -> Result<StatefulTreeIdentity, crate::transition_identity::Error> {
        StatefulTreeIdentity::prepare(
            &self.installation,
            &self.database,
            &self.candidate_path,
            self.candidate_state,
        )
    }

    /// Neither tree may carry a marker. This is the "before marker publication"
    /// half of every guard in this file: a refused preflight must not leave a
    /// published token behind on either side, because a token is a permanent
    /// tree identity and publishing one for a transition that never ran would
    /// bind an identity to a tree that was never activated.
    fn assert_no_marker_published(&self) {
        assert_state_metadata_name_absent(&self.candidate_marker());
        assert_state_metadata_name_absent(&self.live_marker());
    }
}

fn preflight_fixture(summary: &str) -> PreflightFixture {
    let temporary = private_installation_tempdir();
    let mut installation = Installation::open(temporary.path(), None).unwrap();
    let database = db::state::Database::new(":memory:").unwrap();
    let previous_state = database.add(&[], Some(summary), None).unwrap().id;
    let candidate_state = add_cleared_state_with_provenance(&database, summary, 'e');
    prepare_previous_tree(&installation, PreviousKind::Active, previous_state);
    installation.active_state = Some(previous_state);
    install_root_abi_subset(&installation.root, 0);

    let candidate_path = installation.staging_path("usr");
    create_canonical_directory(&candidate_path);
    write_canonical_file(&candidate_path.join(".stateID"), candidate_state.to_string().as_bytes());

    PreflightFixture {
        _temporary: temporary,
        installation,
        database,
        previous_state,
        candidate_state,
        candidate_path,
    }
}

/// The negative control for every guard below.
///
/// Each of those plants one precondition and asserts a refusal. If the bare
/// fixture could not prepare, they would all pass vacuously on whatever
/// unrelated failure the fixture happened to produce, so the clean case has to
/// be pinned explicitly rather than assumed.
#[test]
fn a_clean_preflight_fixture_prepares_and_publishes_both_markers() {
    let fixture = preflight_fixture("clean baseline control");
    let identity = fixture.prepare().expect("a clean baseline must prepare");
    drop(identity);
    assert!(fs::symlink_metadata(fixture.candidate_marker()).unwrap().is_file());
    assert!(fs::symlink_metadata(fixture.live_marker()).unwrap().is_file());
}

#[test]
fn duplicate_permanent_tree_tokens_block_the_exchange_and_retain_both_trees() {
    let fixture = preflight_fixture("duplicate token");

    // Publish the candidate's own marker first, then plant a byte-identical
    // frame on the live tree. Copying an *already-published* frame is what
    // makes both tokens equal without either marker looking forged, so the
    // duplicate-token guard is what refuses rather than a validity check.
    let candidate_store = TreeMarkerStore::open_path(&fixture.candidate_path).unwrap();
    let marker = candidate_store.adopt_or_create_before_journal().unwrap();
    marker.revalidate(&candidate_store).unwrap();
    let frame = fs::read(fixture.candidate_marker()).unwrap();
    drop(marker);
    drop(candidate_store);
    fs::write(fixture.live_marker(), &frame).unwrap();
    fs::set_permissions(fixture.live_marker(), fs::Permissions::from_mode(0o444)).unwrap();

    let error = fixture
        .prepare()
        .err()
        .expect("a token claimed by both trees must refuse the transition");
    assert!(
        matches!(error, crate::transition_identity::Error::DuplicateTreeToken { .. }),
        "expected a duplicate-token refusal, got {error:?}"
    );

    // The refusal is the cheap half of the claim. The substance is that both
    // trees survive it intact: an ambiguous token identifies neither tree, so
    // it cannot license repairing, renaming, or re-marking either side.
    assert_eq!(fs::read(fixture.candidate_marker()).unwrap(), frame);
    assert_eq!(fs::read(fixture.live_marker()).unwrap(), frame);
    assert_eq!(
        fs::read_to_string(fixture.candidate_path.join(".stateID")).unwrap(),
        fixture.candidate_state.to_string()
    );
    assert_eq!(
        fs::read_to_string(fixture.installation.root.join("usr/.stateID")).unwrap(),
        fixture.previous_state.to_string()
    );
    assert_eq!(
        fixture.database.get(fixture.candidate_state).unwrap().id,
        fixture.candidate_state
    );
    assert_canonical_journal_absent(&fixture.installation.root);
}

#[test]
fn unresolved_journal_evidence_blocks_marker_publication_before_activation() {
    let fixture = preflight_fixture("unresolved journal evidence");
    // Opening and dropping the store creates the journal directory without
    // leaving a record, so the bytes below land at the canonical name rather
    // than failing on a missing parent.
    drop(TransitionJournalStore::open(&fixture.installation.root).unwrap());
    let canonical = fixture.canonical_journal();
    let evidence = b"not-a-canonical-transition-record";
    fs::write(&canonical, evidence).unwrap();
    fs::set_permissions(&canonical, fs::Permissions::from_mode(0o600)).unwrap();

    let error = fixture
        .prepare()
        .err()
        .expect("undecodable journal evidence must refuse the transition");

    // Unreadable evidence is not the same claim as a decodable pending record:
    // the baseline cannot be proven clean, so preflight refuses without ever
    // learning which transition the bytes describe.
    assert!(
        matches!(error, crate::transition_identity::Error::Journal { .. }),
        "expected a journal-evidence refusal, got {error:?}"
    );
    fixture.assert_no_marker_published();

    // The bytes are evidence of an unfinished transition. Preflight has no
    // authority to interpret or discard them; that belongs to recovery.
    assert_eq!(fs::read(&canonical).unwrap(), evidence);
}

/// The legacy test above plants undecodable bytes, so it never reaches
/// `UnresolvedJournal` — the guard it is named for. A *decodable* pending
/// record is the case that actually matters in production (a transition that
/// crashed mid-flight), and nothing on the coordinated route covered it: every
/// other `UnresolvedJournal` in the tree belongs to a different error type.
#[test]
fn a_durable_pending_record_blocks_a_second_transition_from_preparing() {
    let fixture = preflight_fixture("pending record");
    let coordinator = fixture
        .prepare()
        .expect("a clean baseline must prepare")
        .begin_transition(StatefulTransitionRequest::ActivateArchived {
            candidate: fixture.candidate_state,
            previous: fixture.previous_state,
            run_system_triggers: false,
            run_boot_sync: false,
        })
        .expect("the first transition must reach Preparing");
    let pending = read_canonical(&fixture.installation.root);
    // Drop the coordinator so the second attempt refuses on the record itself
    // rather than blocking on the canonical lock the first still holds.
    drop(coordinator);

    let error = fixture
        .prepare()
        .err()
        .expect("a durable pending record must refuse a second transition");

    let crate::transition_identity::Error::UnresolvedJournal { transition } = &error else {
        panic!("expected an unresolved-journal refusal, got {error:?}");
    };
    assert_eq!(transition, pending.transition_id.as_str());

    // The pending record is the first transition's only claim on the trees.
    // Refusing must leave it byte-identical for recovery to act on.
    assert_eq!(read_canonical(&fixture.installation.root), pending);
}

#[test]
fn orphan_transition_row_blocks_marker_publication_before_activation() {
    let fixture = preflight_fixture("orphan transition row");
    let transition = TransitionId::generate().unwrap();
    fixture
        .database
        .add_with_transition(&transition, &[], Some("orphan"), None)
        .unwrap();

    let error = fixture
        .prepare()
        .err()
        .expect("an in-flight transition row must refuse the transition");

    let crate::transition_identity::Error::OrphanTransitionRow {
        transition: refused, ..
    } = &error
    else {
        panic!("expected an orphan-transition-row refusal, got {error:?}");
    };
    assert_eq!(refused, transition.as_str());
    fixture.assert_no_marker_published();

    // A clean filesystem baseline is not enough on its own: the database can
    // record an in-flight transition the journal never mentions, and starting a
    // second transition on top of it would lose the first.
    assert_canonical_journal_absent(&fixture.installation.root);
}

fn synthesized_baseline_token(live_usr: &Path) -> String {
    TreeMarkerStore::open_path(live_usr)
        .unwrap()
        .read_for_recovery()
        .unwrap()
        .token()
        .as_str()
        .to_owned()
}

fn assert_marker_only_tree(live_usr: &Path) {
    let entries = fs::read_dir(live_usr)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries, [std::ffi::OsString::from(".cast-tree-id")]);
}

/// A retried first install must adopt the baseline the first attempt
/// synthesized, not mint a fresh one.
///
/// The legacy test reached its second preparation by injecting a failure
/// before the exchange; the failure was only its way of getting two
/// preparations in a row against one installation. Doing that directly
/// isolates the claim, which is about what the *second* preparation does with
/// an existing marker-only `/usr` — a new token there would orphan the durable
/// baseline the first attempt already committed to, and every later identity
/// check would be comparing against a tree that no record describes.
#[test]
fn a_first_install_retry_adopts_the_exact_marker_only_synthesized_baseline() {
    let (fixture, identity, authority) = fixture_parts(
        CandidateKind::NewState,
        PreviousKind::SynthesizedEmpty,
        true,
        false,
    );
    let live_usr = fixture.installation.root.join("usr");
    let baseline = synthesized_baseline_token(&live_usr);
    assert_marker_only_tree(&live_usr);
    drop(identity);
    drop(authority);

    // Re-stage the candidate exactly as a retry would, then prepare again.
    fs::remove_dir_all(fixture.installation.staging_path("usr")).unwrap();
    let (identity, authority) = reacquire_new_state(&fixture);

    assert_eq!(
        synthesized_baseline_token(&live_usr),
        baseline,
        "the retry minted a new baseline token instead of adopting the durable one"
    );
    assert_marker_only_tree(&live_usr);
    drop(identity);
    drop(authority);
}
