/// The generation a phase sits at is fixed by which phases the record's options
/// make it traverse. These pin the values the staged boot chain previously
/// hard-coded per operation (`plans/future_impl.md` §1.1d).
#[test]
fn generation_is_derived_from_the_phases_the_options_traverse() {
    use super::validation::expected_forward_generation;

    // ActiveReblit repairs in place: no fresh allocation, no predecessor
    // archive. These are the literals the staged chain used.
    let active_reblit = reblit_record(Phase::BootSyncStarted);
    assert_eq!(
        expected_forward_generation(&active_reblit, ForwardPhase::BootSyncComplete),
        Some(12),
        "matches the former ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_GENERATION",
    );
    assert_eq!(
        expected_forward_generation(&active_reblit, ForwardPhase::CommitDecided),
        Some(13),
        "matches the former ACTIVE_REBLIT_COMMIT_DECIDED_GENERATION",
    );

    // NewState that archives a predecessor additionally traverses
    // FreshStateAllocating/Allocated and PreviousArchiveIntent/Archived, so the
    // same phases sit four generations higher.
    let new_state = new_state_record(Phase::BootSyncStarted);
    assert!(new_state.options.archive_previous);
    assert_eq!(
        expected_forward_generation(&new_state, ForwardPhase::BootSyncComplete),
        Some(16),
    );
    assert_eq!(
        expected_forward_generation(&new_state, ForwardPhase::CommitDecided),
        Some(17),
    );

    // Every step advances by exactly one.
    assert_eq!(
        expected_forward_generation(&new_state, ForwardPhase::CommitCleanupComplete),
        Some(18),
    );
    assert_eq!(expected_forward_generation(&new_state, ForwardPhase::Complete), Some(19));
    assert_eq!(expected_forward_generation(&new_state, ForwardPhase::Preparing), Some(1));
}

#[test]
fn a_phase_the_options_skip_has_no_generation() {
    use super::validation::expected_forward_generation;

    // Boot disabled: the boot phases are never traversed, so asking for their
    // generation must not invent one.
    let mut no_boot = new_state_record(Phase::SystemTriggersComplete);
    no_boot.options.run_boot_sync = false;
    no_boot.boot_publication_receipts = None;
    assert_eq!(expected_forward_generation(&no_boot, ForwardPhase::BootSyncStarted), None);
    assert_eq!(expected_forward_generation(&no_boot, ForwardPhase::BootSyncComplete), None);
    // ...and the phases it does reach shift down accordingly.
    assert_eq!(
        expected_forward_generation(&no_boot, ForwardPhase::CommitDecided),
        Some(15),
    );

    // No archive: the predecessor phases are skipped too.
    let mut no_archive = new_state_record(Phase::SystemTriggersComplete);
    no_archive.options.archive_previous = false;
    assert_eq!(
        expected_forward_generation(&no_archive, ForwardPhase::PreviousArchived),
        None
    );
}
