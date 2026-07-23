//! Stable comparison of retained ActiveReblit database state.

use crate::{State, state};

pub(super) fn same_state_snapshot(expected: &State, actual: &State) -> bool {
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
