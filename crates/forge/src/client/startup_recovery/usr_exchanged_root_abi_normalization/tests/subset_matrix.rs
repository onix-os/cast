use crate::{
    client::startup_reconciliation::{
        reset_usr_exchanged_root_abi_effect_counts, usr_exchanged_root_abi_complete_sync_attempts,
        usr_exchanged_root_abi_publication_attempts,
    },
    transition_journal::Phase,
};

use super::fixture::{Fixture, OperationKind, SourceCase, pending};

#[test]
fn startup_usr_exchanged_root_abi_all_canonical_subsets_converge_without_phase_skip() {
    for kind in OperationKind::ALL {
        for mask in 0_u8..32 {
            let fixture = Fixture::new(kind, SourceCase::ExchangedPost);
            fixture.set_root_abi_subset(mask);
            let source_bytes = fixture.canonical_bytes();
            let database_before = fixture.database_snapshot();
            reset_usr_exchanged_root_abi_effect_counts();

            let first = fixture.enter();
            if mask == 31 {
                assert_eq!(pending(&first).phase(), Phase::RollbackDecided, "{kind:?} mask={mask:#07b}");
                assert_eq!(usr_exchanged_root_abi_publication_attempts(), 0);
                assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 1);
            } else {
                assert_eq!(pending(&first).phase(), Phase::UsrExchanged, "{kind:?} mask={mask:#07b}");
                assert_eq!(fixture.canonical_bytes(), source_bytes, "{kind:?} mask={mask:#07b}");
                assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
                assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 0);
                fixture.assert_complete_root_abi();

                let second = fixture.enter();
                assert_eq!(pending(&second).phase(), Phase::RollbackDecided, "{kind:?} mask={mask:#07b}");
                assert_eq!(usr_exchanged_root_abi_publication_attempts(), 1);
                assert_eq!(usr_exchanged_root_abi_complete_sync_attempts(), 1);
            }
            fixture.assert_complete_root_abi();
            assert_eq!(fixture.database_snapshot(), database_before, "{kind:?} mask={mask:#07b}");
        }
    }
}
