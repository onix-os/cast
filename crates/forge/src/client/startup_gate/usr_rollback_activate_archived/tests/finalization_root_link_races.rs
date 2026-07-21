//! RootLinks terminal evidence races at capture, final admission, and absence proof.

use std::{fs, path::Path};

use crate::client::{
    startup_reconciliation::arm_between_usr_rollback_activate_archived_finalization_database_captures,
    startup_recovery::{
        arm_after_usr_rollback_activate_archived_finalization_delete,
        arm_before_usr_rollback_activate_archived_finalization_final_revalidation,
    },
};
use crate::transition_journal::RollbackActionOutcome;

use super::{
    super::candidate_test_support::CandidateSource,
    support::{
        CandidateOutcome, Epoch, RouteFixture, assert_canonical_absent,
        assert_finalization_dispatch_error, assert_pending_phase, candidate_move_count, enter_route,
        persist_rollback_complete, reset_candidate_observers,
    },
};

const ROOT_ABI: [(&str, &str); 5] = [
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib32", "usr/lib32"),
    ("lib64", "usr/lib"),
];

#[derive(Clone, Copy, Debug)]
enum EvidenceSeam {
    Capture,
    FinalAdmission,
    PostDelete,
}

#[test]
fn startup_activate_archived_finalization_root_links_rejects_all_five_link_races_at_each_evidence_seam() {
    let mut cases = 0;
    for seam in [
        EvidenceSeam::Capture,
        EvidenceSeam::FinalAdmission,
        EvidenceSeam::PostDelete,
    ] {
        for (removed_name, _) in ROOT_ABI {
            let fixture = RouteFixture::new(
                Epoch::Current,
                CandidateSource::RootLinksComplete,
                RollbackActionOutcome::Applied,
                CandidateOutcome::AlreadySatisfied,
            );
            let terminal = persist_rollback_complete(&fixture);
            assert_eq!(terminal.generation, 12);
            let database_before = fixture.database_snapshot();
            let root = fixture.fixture.fixture.installation.root.clone();
            let removed = root.join(removed_name);
            arm_at(seam, move || fs::remove_file(removed).unwrap());
            reset_candidate_observers();

            let error = enter_route(&fixture);

            match seam {
                EvidenceSeam::Capture => {
                    assert_pending_phase(&error, crate::transition_journal::Phase::RollbackComplete);
                    assert_eq!(fixture.canonical_record(), terminal);
                }
                EvidenceSeam::FinalAdmission => {
                    assert_finalization_dispatch_error(&error);
                    assert_eq!(fixture.canonical_record(), terminal);
                }
                EvidenceSeam::PostDelete => {
                    assert_finalization_dispatch_error(&error);
                    assert_canonical_absent(&root);
                }
            }
            assert_root_abi_except(&root, removed_name);
            assert_eq!(fixture.database_snapshot(), database_before);
            fixture.assert_exact_archived_topology();
            assert_eq!(candidate_move_count(), 0);
            cases += 1;
        }
    }
    assert_eq!(cases, 15);
}

fn arm_at(seam: EvidenceSeam, hook: impl FnOnce() + 'static) {
    match seam {
        EvidenceSeam::Capture => {
            arm_between_usr_rollback_activate_archived_finalization_database_captures(hook)
        }
        EvidenceSeam::FinalAdmission => {
            arm_before_usr_rollback_activate_archived_finalization_final_revalidation(hook)
        }
        EvidenceSeam::PostDelete => {
            arm_after_usr_rollback_activate_archived_finalization_delete(hook)
        }
    }
}

fn assert_root_abi_except(root: &Path, removed_name: &str) {
    for (name, target) in ROOT_ABI {
        let path = root.join(name);
        if name == removed_name {
            assert!(!path.exists());
        } else {
            assert_eq!(fs::read_link(path).unwrap(), Path::new(target));
        }
    }
}
