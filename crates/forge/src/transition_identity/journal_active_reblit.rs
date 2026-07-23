//! ActiveReblit snapshot proof under an already-durable coordinator journal.
//!
//! Legacy snapshot helpers continue to require a clean journal baseline.  This
//! sealed counterpart is reachable only from the private exchange effect and
//! substitutes exact canonical-record validation performed by that owner.

use crate::{Installation, State, state};

use super::{Error, StatefulTreeIdentity, journal_coordinator::UsrExchangeEffectSeal};

impl StatefulTreeIdentity {
    pub(super) fn verify_journal_active_reblit_snapshot(
        &self,
        _seal: &UsrExchangeEffectSeal,
        installation: &Installation,
        expected: &State,
        live: bool,
    ) -> Result<(), Error> {
        self.require_exact_active_reblit_state(expected)?;
        if installation.active_state != Some(expected.id) {
            return Err(Error::ActiveReblitSelectionChanged {
                expected: i32::from(expected.id),
                actual: installation.active_state.map(i32::from),
            });
        }

        let path = if live {
            installation.root.join("usr")
        } else {
            installation.staging_path("usr")
        };
        let _coordinator_authority = _seal;
        self.require_active_previous_slot_unchanged_with_journal(_seal, installation, expected.id)
            .map_err(|source| Error::ActivePreviousSlotParking {
                source: Box::new(source),
            })?;
        self.verify_candidate_named_with_state_id(&path)?;
        self.require_exact_active_reblit_state(expected)?;
        self.require_active_previous_slot_unchanged_with_journal(_seal, installation, expected.id)
            .map_err(|source| Error::ActivePreviousSlotParking {
                source: Box::new(source),
            })?;
        self.verify_candidate_named_with_state_id(&path)
    }

    fn require_exact_active_reblit_state(&self, expected: &State) -> Result<(), Error> {
        let actual = self
            .state_database
            .get(expected.id)
            .map_err(|source| Error::ActiveReblitStateLookup {
                state: i32::from(expected.id),
                source,
            })?;
        if same_state_snapshot(expected, &actual) {
            Ok(())
        } else {
            Err(Error::ActiveReblitStateChanged {
                state: i32::from(expected.id),
            })
        }
    }
}

fn same_state_snapshot(expected: &State, actual: &State) -> bool {
    let mut expected_selections = expected.selections.clone();
    let mut actual_selections = actual.selections.clone();
    let sort = |selections: &mut Vec<state::Selection>| {
        selections.sort_by(|left, right| {
            left.package
                .cmp(&right.package)
                .then(left.explicit.cmp(&right.explicit))
                .then(left.reason.cmp(&right.reason))
        });
    };
    sort(&mut expected_selections);
    sort(&mut actual_selections);
    expected.id == actual.id
        && expected.summary == actual.summary
        && expected.description == actual.description
        && expected.created == actual.created
        && expected.kind == actual.kind
        && expected_selections == actual_selections
}
