//! Real startup-entry routing for exact and deliberately deferred boot completion.

use std::fs;

use crate::{
    client::{MutableSystemCapabilities, MutableSystemCapabilitiesTestSeal},
    db,
    state,
    transition_journal::Phase,
};

use super::{
    boot_sync_complete_support::{
        BootSyncCompleteReadOnlySnapshot, boot_sync_complete_fixture, exact_commit_decided,
        exact_promoted_receipt_state, legacy_boot_sync_complete_fixture,
    },
    support::{
        Epoch, assert_complete_route_journal_only, assert_pending_phase, enter, enter_boot,
        reset_complete_route_effect_observers,
    },
};

#[test]
fn startup_boot_sync_complete_current_and_historical_advance_once_to_exact_commit_decided() {
    for epoch in Epoch::ALL {
        let fixture = boot_sync_complete_fixture(epoch, true);
        let source = fixture.fixture.source.clone();
        let expected = exact_commit_decided(&fixture);
        let receipt_before = exact_promoted_receipt_state(&fixture);
        let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
        reset_complete_route_effect_observers();

        let first = enter_boot(&fixture);

        assert_pending_phase(&first, Phase::CommitDecided);
        assert_eq!(fixture.fixture.canonical_record(), expected);
        assert_eq!(expected.generation, source.generation + 1);
        assert_eq!(expected.rollback, None);
        assert_eq!(exact_promoted_receipt_state(&fixture), receipt_before);
        read_only.assert_unchanged(&fixture);
        assert_complete_route_journal_only();

        // The new successor is returned immediately. A later entry still sees
        // that exact record; this slice performs no cleanup or redispatch.
        let second = enter_boot(&fixture);
        assert_pending_phase(&second, Phase::CommitDecided);
        assert_eq!(fixture.fixture.canonical_record(), expected);
        assert_complete_route_journal_only();
    }
}

#[test]
fn startup_unpromoted_boot_sync_complete_stays_exactly_pending_and_never_rolls_back() {
    let fixture = boot_sync_complete_fixture(Epoch::Current, false);
    let source = fixture.fixture.source.clone();
    let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
    reset_complete_route_effect_observers();

    let error = enter_boot(&fixture);

    assert_pending_phase(&error, Phase::BootSyncComplete);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(source.rollback, None);
    read_only.assert_unchanged(&fixture);
    assert_complete_route_journal_only();
}

#[test]
fn startup_legacy_v2_boot_sync_complete_without_receipt_pair_stays_forward_pending() {
    let fixture = legacy_boot_sync_complete_fixture(Epoch::Historical, 2);
    let source = fixture.fixture.source.clone();
    let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
    reset_complete_route_effect_observers();

    let error = enter_boot(&fixture);

    assert_pending_phase(&error, Phase::BootSyncComplete);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(source.version, 2);
    assert_eq!(source.boot_publication_receipt_correlation().unwrap(), None);
    assert_eq!(source.rollback, None);
    read_only.assert_unchanged(&fixture);
    assert_complete_route_journal_only();
}

#[test]
fn startup_stable_active_selection_mismatch_stays_boot_sync_complete_without_rollback() {
    let mut fixture = boot_sync_complete_fixture(Epoch::Current, true);
    let source = fixture.fixture.source.clone();
    let other = state::Id::from(i32::from(fixture.fixture.candidate_state) + 100);
    fixture.fixture.installation.active_state = Some(other);
    fs::write(
        fixture.fixture.installation.root.join("usr/.stateID"),
        i32::from(other).to_string(),
    )
    .unwrap();
    let layout_database = db::layout::Database::new(":memory:").unwrap();
    let system = MutableSystemCapabilities::from_test_parts(
        &MutableSystemCapabilitiesTestSeal::new(),
        fixture.fixture.installation.clone(),
        fixture.fixture.database.clone(),
        layout_database,
    );
    let read_only = BootSyncCompleteReadOnlySnapshot::capture(&fixture);
    reset_complete_route_effect_observers();

    let error = enter(&system);

    assert_pending_phase(&error, Phase::BootSyncComplete);
    assert_eq!(fixture.fixture.canonical_record(), source);
    assert_eq!(source.rollback, None);
    read_only.assert_unchanged(&fixture);
    assert_complete_route_journal_only();
}
