use std::path::Path;

use super::super::*;

fn stat_path() -> &'static Path {
    Path::new("cgroup.stat")
}

fn assert_topology_error(
    result: Result<()>,
    expected_descendants: u64,
    maximum_dying_descendants: u64,
    descendants: u64,
    dying_descendants: u64,
) {
    assert!(matches!(
        result,
        Err(CgroupError::DelegationTopology {
            expected_descendants: expected,
            maximum_dying_descendants: maximum,
            descendants: found,
            dying_descendants: dying,
            ..
        }) if expected == expected_descendants
            && maximum == maximum_dying_descendants
            && found == descendants
            && dying == dying_descendants
    ));
}

#[test]
fn root_admission_accepts_only_the_reserved_retired_budget() {
    assert_eq!(MAX_RETIRED_CGROUPS, 64);
    assert_eq!(MAX_RETIRED_CGROUPS_AT_ADMISSION, 63);

    validate_descendant_topology(1, 0, 1, MAX_RETIRED_CGROUPS_AT_ADMISSION, stat_path()).unwrap();
    validate_descendant_topology(
        1,
        MAX_RETIRED_CGROUPS_AT_ADMISSION,
        1,
        MAX_RETIRED_CGROUPS_AT_ADMISSION,
        stat_path(),
    )
    .unwrap();
    assert_topology_error(
        validate_descendant_topology(1, MAX_RETIRED_CGROUPS, 1, MAX_RETIRED_CGROUPS_AT_ADMISSION, stat_path()),
        1,
        MAX_RETIRED_CGROUPS_AT_ADMISSION,
        1,
        MAX_RETIRED_CGROUPS,
    );
}

#[test]
fn cleanup_admits_the_final_retired_leaf_but_not_an_extra_one() {
    validate_descendant_topology(1, MAX_RETIRED_CGROUPS, 1, MAX_RETIRED_CGROUPS, stat_path()).unwrap();
    assert_topology_error(
        validate_descendant_topology(1, MAX_RETIRED_CGROUPS + 1, 1, MAX_RETIRED_CGROUPS, stat_path()),
        1,
        MAX_RETIRED_CGROUPS,
        1,
        MAX_RETIRED_CGROUPS + 1,
    );
}

#[test]
fn visible_descendant_mismatch_is_rejected_at_every_retired_count() {
    for (expected_descendants, wrong_counts) in [(1, &[0, 2, 3][..]), (2, &[0, 1, 3][..])] {
        for descendants in wrong_counts {
            for dying_descendants in [0, MAX_RETIRED_CGROUPS_AT_ADMISSION] {
                assert_topology_error(
                    validate_descendant_topology(
                        *descendants,
                        dying_descendants,
                        expected_descendants,
                        MAX_RETIRED_CGROUPS_AT_ADMISSION,
                        stat_path(),
                    ),
                    expected_descendants,
                    MAX_RETIRED_CGROUPS_AT_ADMISSION,
                    *descendants,
                    dying_descendants,
                );
            }
        }
    }
}

#[test]
fn strict_child_topology_rejects_any_dying_descendant() {
    validate_descendant_topology(0, 0, 0, 0, stat_path()).unwrap();
    assert_topology_error(validate_descendant_topology(0, 1, 0, 0, stat_path()), 0, 0, 0, 1);
}

#[test]
fn one_live_leaf_reserves_the_last_sequential_cleanup_slot() {
    for completed_execution in 0..=MAX_RETIRED_CGROUPS_AT_ADMISSION {
        validate_descendant_topology(2, completed_execution, 2, MAX_RETIRED_CGROUPS_AT_ADMISSION, stat_path()).unwrap();
    }
    validate_descendant_topology(1, MAX_RETIRED_CGROUPS, 1, MAX_RETIRED_CGROUPS, stat_path()).unwrap();
}
