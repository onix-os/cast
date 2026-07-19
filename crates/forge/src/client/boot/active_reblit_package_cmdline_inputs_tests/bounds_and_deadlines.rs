use std::cell::Cell;

use super::{support::*, *};

fn prepare_with_policy<'stone>(
    stone: &'stone PreparedActiveReblitStoneBootInputs,
    policy: PackageCmdlinePolicy,
) -> Result<PreparedActiveReblitPackageCmdlineInputs<'stone>, ActiveReblitPackageCmdlineInputsError> {
    binding::prepare_with_policy_until(stone, policy, future_deadline())
}

#[test]
fn entry_and_per_source_bounds_admit_n_and_reject_n_plus_one() {
    let fixture = PackageCmdlineFixture::new(fixture_entries([
        ("lib/kernel/6.12/10-four.cmdline".to_owned(), b"four".to_vec()),
        ("lib/kernel/cmdline.d/20-four.cmdline".to_owned(), b"also".to_vec()),
    ]));
    let stone = fixture.ready();

    let mut exact = PackageCmdlinePolicy::production();
    exact.max_entries = 2;
    exact.max_source_bytes = 4;
    assert_eq!(prepare_with_policy(&stone, exact).unwrap().entries().len(), 2);

    let mut count_too_small = exact;
    count_too_small.max_entries = 1;
    assert!(matches!(
        prepare_with_policy(&stone, count_too_small),
        Err(ActiveReblitPackageCmdlineInputsError::EntryCountLimit { limit: 1, actual: 2 })
    ));

    let mut source_too_small = exact;
    source_too_small.max_source_bytes = 3;
    assert!(matches!(
        prepare_with_policy(&stone, source_too_small),
        Err(ActiveReblitPackageCmdlineInputsError::SourceByteLimit {
            limit: 3,
            actual: 4,
            ..
        })
    ));
}

#[test]
fn aggregate_source_bound_counts_every_reference_to_a_shared_snapshot() {
    let shared = b"same";
    let fixture = PackageCmdlineFixture::new(fixture_entries([
        ("lib/kernel/6.12/10-shared.cmdline".to_owned(), shared.to_vec()),
        ("lib/kernel/cmdline.d/20-shared.cmdline".to_owned(), shared.to_vec()),
    ]));
    let stone = fixture.ready();
    let mut exact = PackageCmdlinePolicy::production();
    exact.max_total_bytes = shared.len() * 2;
    let prepared = prepare_with_policy(&stone, exact).unwrap();
    assert_eq!(prepared.total_source_bytes(), shared.len() * 2);

    let mut too_small = exact;
    too_small.max_total_bytes -= 1;
    assert!(matches!(
        prepare_with_policy(&stone, too_small),
        Err(ActiveReblitPackageCmdlineInputsError::TotalByteLimit { limit, actual })
            if limit == shared.len() * 2 - 1 && actual == shared.len() * 2
    ));
}

#[test]
fn exact_preparation_work_is_admitted_and_n_plus_one_is_rejected() {
    let fixture = one_global(b"one\ntwo".as_slice());
    let stone = fixture.ready();
    let observed = PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline())
        .unwrap()
        .preparation_work();
    assert!(observed > 0);

    let mut exact = PackageCmdlinePolicy::production();
    exact.max_work = observed;
    assert_eq!(prepare_with_policy(&stone, exact).unwrap().preparation_work(), observed);

    let mut too_small = exact;
    too_small.max_work -= 1;
    assert!(matches!(
        prepare_with_policy(&stone, too_small),
        Err(ActiveReblitPackageCmdlineInputsError::WorkLimit { limit, actual })
            if limit == observed - 1 && actual == observed
    ));
}

#[test]
fn expired_caller_deadline_is_rejected_at_entry_and_revalidation() {
    let fixture = one_global(b"quiet".as_slice());
    let stone = fixture.ready();
    let expired = Instant::now() - Duration::from_millis(1);
    assert!(matches!(
        PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, expired),
        Err(ActiveReblitPackageCmdlineInputsError::DeadlineExceeded {
            checkpoint: "coordinator entry"
        })
    ));

    let prepared = PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline()).unwrap();
    assert!(matches!(
        prepared.revalidate_until(expired),
        Err(ActiveReblitPackageCmdlineInputsError::DeadlineExceeded {
            checkpoint: "coordinator entry"
        })
    ));
}

#[test]
fn one_caller_deadline_is_not_replaced_between_sources_or_at_terminal_return() {
    let fixture = PackageCmdlineFixture::new(fixture_entries([
        ("lib/kernel/6.12/10-first.cmdline".to_owned(), b"first=yes".to_vec()),
        (
            "lib/kernel/cmdline.d/20-second.cmdline".to_owned(),
            b"second=yes".to_vec(),
        ),
    ]));
    let stone = fixture.ready();
    let slept = Cell::new(false);
    let deadline = Instant::now() + Duration::from_millis(10);
    let result = binding::prepare_with_policy_until_and_checkpoint(
        &stone,
        PackageCmdlinePolicy::production(),
        deadline,
        |checkpoint| {
            if !slept.get()
                && matches!(
                    checkpoint,
                    binding::PackageCmdlineCheckpoint::SourceAuthenticated { .. }
                )
            {
                slept.set(true);
                std::thread::sleep(Duration::from_millis(30));
            }
        },
    );
    assert!(slept.get());
    assert!(matches!(
        result,
        Err(ActiveReblitPackageCmdlineInputsError::DeadlineExceeded {
            checkpoint: "source authentication checkpoint"
        })
    ));

    let terminal_deadline = Instant::now() + Duration::from_millis(10);
    let terminal = binding::prepare_with_policy_until_and_checkpoint(
        &stone,
        PackageCmdlinePolicy::production(),
        terminal_deadline,
        |checkpoint| {
            if matches!(checkpoint, binding::PackageCmdlineCheckpoint::PreparedValueMaterialized) {
                std::thread::sleep(Duration::from_millis(30));
            }
        },
    );
    assert!(matches!(
        terminal,
        Err(ActiveReblitPackageCmdlineInputsError::DeadlineExceeded {
            checkpoint: "terminal package command-line checkpoint"
        })
    ));
}

#[test]
fn normalized_scalar_deadline_is_checked_after_materialization() {
    let deadline = Instant::now() + Duration::from_millis(10);
    let mut budget = PackageCmdlineBudget::new(PackageCmdlinePolicy::production(), deadline).unwrap();
    let result =
        normalization::normalize_package_cmdline_with_checkpoint(4, b"quiet splash", &mut budget, |checkpoint| {
            if matches!(
                checkpoint,
                normalization::PackageCmdlineNormalizationCheckpoint::Materialized
            ) {
                std::thread::sleep(Duration::from_millis(30));
            }
        });
    assert!(matches!(
        result,
        Err(ActiveReblitPackageCmdlineInputsError::DeadlineExceeded {
            checkpoint: "normalized command-line materialization"
        })
    ));
}

#[test]
fn production_limits_and_sort_reservation_are_explicit() {
    let policy = PackageCmdlinePolicy::production();
    assert_eq!(policy.max_entries, 8_192);
    assert_eq!(policy.max_source_bytes, 64 * 1024);
    assert_eq!(policy.max_total_bytes, 16 * 1024 * 1024);
    assert_eq!(policy.max_work, 1_000_000);
    assert_eq!(MAX_PACKAGE_CMDLINE_INTERRUPTED_RETRIES, 1_024);
    assert_eq!(conservative_sort_work(0), 0);
    assert_eq!(conservative_sort_work(1), 0);
    assert_eq!(conservative_sort_work(8), 8 * 3 * SORT_WORK_PER_ELEMENT_LEVEL);
}

#[test]
fn interrupted_reads_admit_the_exact_retry_limit_and_reject_the_next_attempt() {
    let fixture = one_global(b"x".as_slice());
    let stone = fixture.ready();
    let asset = stone
        .assets()
        .find(|asset| matches!(asset.role(), BootAssetRole::GlobalCmdline))
        .expect("fixture owns one global command-line source");

    let accepted_calls = Cell::new(0usize);
    let mut accepted_budget = PackageCmdlineBudget::new(PackageCmdlinePolicy::production(), future_deadline()).unwrap();
    let bytes =
        binding::read_exact_source_at_with(asset.descriptor(), 1, 7, &mut accepted_budget, |_, buffer, offset| {
            assert_eq!(offset, 0);
            let call = accepted_calls.get();
            accepted_calls.set(call + 1);
            if call < MAX_PACKAGE_CMDLINE_INTERRUPTED_RETRIES {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                buffer[0] = b'x';
                Ok(1)
            }
        })
        .unwrap();
    assert_eq!(bytes, b"x");
    assert_eq!(accepted_calls.get(), MAX_PACKAGE_CMDLINE_INTERRUPTED_RETRIES + 1);

    let rejected_calls = Cell::new(0usize);
    let mut rejected_budget = PackageCmdlineBudget::new(PackageCmdlinePolicy::production(), future_deadline()).unwrap();
    let error = binding::read_exact_source_at_with(asset.descriptor(), 1, 7, &mut rejected_budget, |_, _, _| {
        rejected_calls.set(rejected_calls.get() + 1);
        Err(io::Error::from(io::ErrorKind::Interrupted))
    })
    .unwrap_err();
    assert!(matches!(
        error,
        ActiveReblitPackageCmdlineInputsError::ReadSource {
            binding_index: 7,
            source,
        } if source.kind() == io::ErrorKind::Interrupted
    ));
    assert_eq!(rejected_calls.get(), MAX_PACKAGE_CMDLINE_INTERRUPTED_RETRIES + 1);
}

#[test]
fn adversarial_last_state_position_lookup_is_precomputed_and_charged_exactly() {
    let states = [1, 2, 3, 4, 5].map(state::Id::from);
    let mut exact_policy = PackageCmdlinePolicy::production();
    exact_policy.max_work = states.len();
    let mut exact = PackageCmdlineBudget::new(exact_policy, future_deadline()).unwrap();
    assert_eq!(
        binding::validated_state_position(&states, states[4], 17, &mut exact).unwrap(),
        4
    );
    assert_eq!(exact.work, states.len());

    let mut too_small_policy = exact_policy;
    too_small_policy.max_work -= 1;
    let mut too_small = PackageCmdlineBudget::new(too_small_policy, future_deadline()).unwrap();
    assert!(matches!(
        binding::validated_state_position(&states, states[4], 17, &mut too_small),
        Err(ActiveReblitPackageCmdlineInputsError::WorkLimit { limit, actual })
            if limit == states.len() - 1 && actual == states.len()
    ));
}
