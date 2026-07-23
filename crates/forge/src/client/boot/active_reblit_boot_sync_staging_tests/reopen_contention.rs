use std::{
    sync::mpsc::{self, TryRecvError},
    time::Duration,
};

use super::*;

#[test]
fn reconciliation_never_waits_behind_a_writer_blocked_journal_contender() {
    with_bound_staging_plan!(|fixture, plan, inventory, _claims| {
        let (journal, predecessor, binding) =
            exact_boot_sync_journal(&fixture.installation);
        let root = fixture.installation.root.clone();
        let (journal_sender, journal_receiver) = mpsc::channel();
        let (writer_sender, writer_receiver) = mpsc::channel();
        let contender = std::thread::spawn(move || {
            let journal = TransitionJournalStore::open(&root).unwrap();
            journal_sender.send(()).unwrap();
            let reservation = CoordinatorActiveStateReservation::acquire().unwrap();
            writer_sender.send(()).unwrap();
            drop(reservation);
            drop(journal);
        });
        arm_after_old_journal_drop_before_reopen(move || {
            journal_receiver
                .recv_timeout(Duration::from_secs(120))
                .unwrap();
        });
        arm_next_temporary_sync_fault();

        let error = stage_with_retained_stores(
            &fixture.installation,
            &fixture.state_db,
            &plan,
            &inventory,
            journal,
            predecessor,
            binding,
        )
        .unwrap_err();

        assert_temporary_sync_fault_consumed();
        assert!(matches!(
            error,
            ActiveReblitBootSyncStagingError::JournalAdvanceAndReconciliation {
                reconciliation: ActiveReblitBootSyncReconciliationError::Reopen(_),
                ..
            },
        ));
        writer_receiver
            .recv_timeout(Duration::from_secs(120))
            .unwrap();
        contender.join().unwrap();
        assert!(matches!(
            writer_receiver.try_recv(),
            Err(TryRecvError::Disconnected),
        ));
    });
}
