//! Generation-18 RootLinks evidence races across all five merged-/usr links.

use std::{fs, path::Path};

use crate::{
    client::{
        active_state_snapshot::ActiveStateReservation,
        startup_reconciliation::{
            UsrRollbackFinalizationAdmission,
            arm_between_usr_rollback_finalization_database_captures,
        },
        startup_recovery::{
            UsrRollbackFinalizationError, arm_after_usr_rollback_finalization_delete,
            arm_before_usr_rollback_finalization_final_revalidation, finalize_usr_rollback,
        },
    },
    transition_journal::RollbackActionOutcome,
};

use super::support::{CandidateResult, FinalizationFixture, FreshDbOutcome, Source};

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
fn startup_usr_rollback_finalization_root_links_rejects_all_five_link_races_at_each_evidence_seam() {
    let mut cases = 0;
    for seam in [
        EvidenceSeam::Capture,
        EvidenceSeam::FinalAdmission,
        EvidenceSeam::PostDelete,
    ] {
        for (removed_name, _) in ROOT_ABI {
            let fixture = FinalizationFixture::new(
                FreshDbOutcome::Applied,
                Source::RootLinksComplete,
                RollbackActionOutcome::Applied,
                CandidateResult::AlreadySatisfied,
            );
            assert_eq!(fixture.source.generation, 18);
            let database_before = fixture.database_snapshot();
            let root = fixture.installation().root.clone();
            let removed = root.join(removed_name);
            let journal = fixture.open_journal();
            let reservation = ActiveStateReservation::acquire().unwrap();

            match seam {
                EvidenceSeam::Capture => {
                    arm_between_usr_rollback_finalization_database_captures(move || {
                        fs::remove_file(removed).unwrap();
                    });
                    let admission = fixture.capture(&journal, &reservation).unwrap();
                    assert!(matches!(admission, UsrRollbackFinalizationAdmission::Deferred));
                    assert_eq!(fixture.canonical_record(), fixture.source);
                }
                EvidenceSeam::FinalAdmission => {
                    let authority = fixture.capture_ready(&journal, &reservation);
                    arm_before_usr_rollback_finalization_final_revalidation(move || {
                        fs::remove_file(removed).unwrap();
                    });
                    let error = finalize_usr_rollback(journal, authority).unwrap_err();
                    assert!(matches!(error, UsrRollbackFinalizationError::Authority(_)));
                    assert_eq!(fixture.canonical_record(), fixture.source);
                }
                EvidenceSeam::PostDelete => {
                    let authority = fixture.capture_ready(&journal, &reservation);
                    arm_after_usr_rollback_finalization_delete(move || {
                        fs::remove_file(removed).unwrap();
                    });
                    let error = finalize_usr_rollback(journal, authority).unwrap_err();
                    assert!(matches!(error, UsrRollbackFinalizationError::PostDeleteAuthority(_)));
                    assert!(!root.join(".cast/journal/state-transition").exists());
                }
            }
            assert_root_abi_except(&root, removed_name);
            assert_eq!(fixture.database_snapshot(), database_before);
            fixture.route.fixture.assert_exact_joint_absence();
            fixture.assert_no_second_removal();
            cases += 1;
        }
    }
    assert_eq!(cases, 15);
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
