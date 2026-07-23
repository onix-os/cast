use std::{
    cell::Cell,
    io,
    time::{Duration, Instant},
};

use super::super::super::gpt_partition_device::FixtureGptPartitionDeviceLimits;
use super::support::{
    FixtureInput, FixtureObserver, ObservationFields, PRODUCTION_LIMITS, authenticate, authenticate_with_clock,
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

#[test]
fn observation_limit_admits_exact_n_and_rejects_n_minus_one() {
    let mut exact = FixtureObserver::stable(ObservationFields::standard());
    authenticate(&mut exact, FixtureInput::standard(), PRODUCTION_LIMITS, deadline()).unwrap();
    assert_eq!(exact.calls(), 2);

    let mut short = FixtureObserver::stable(ObservationFields::standard());
    let error = authenticate(
        &mut short,
        FixtureInput::standard(),
        FixtureGptPartitionDeviceLimits {
            observation_calls: 1,
            work_units: 45,
        },
        deadline(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(short.calls(), 1);
}

#[test]
fn work_limit_admits_exact_n_and_rejects_n_minus_one() {
    let mut exact = FixtureObserver::stable(ObservationFields::standard());
    authenticate(&mut exact, FixtureInput::standard(), PRODUCTION_LIMITS, deadline()).unwrap();

    let mut short = FixtureObserver::stable(ObservationFields::standard());
    let error = authenticate(
        &mut short,
        FixtureInput::standard(),
        FixtureGptPartitionDeviceLimits {
            observation_calls: 2,
            work_units: 44,
        },
        deadline(),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(short.calls(), 2);
}

#[test]
fn zero_and_above_production_limits_fail_before_any_observation() {
    for limits in [
        FixtureGptPartitionDeviceLimits {
            observation_calls: 0,
            work_units: 45,
        },
        FixtureGptPartitionDeviceLimits {
            observation_calls: 2,
            work_units: 0,
        },
        FixtureGptPartitionDeviceLimits {
            observation_calls: 3,
            work_units: 45,
        },
        FixtureGptPartitionDeviceLimits {
            observation_calls: 2,
            work_units: 46,
        },
    ] {
        let mut observer = FixtureObserver::stable(ObservationFields::standard());
        let error = authenticate(&mut observer, FixtureInput::standard(), limits, deadline()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(observer.calls(), 0);
    }
}

#[test]
fn expired_initial_deadline_fails_before_any_observation() {
    let mut observer = FixtureObserver::stable(ObservationFields::standard());
    let error = authenticate(
        &mut observer,
        FixtureInput::standard(),
        PRODUCTION_LIMITS,
        Instant::now() - Duration::from_millis(1),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(observer.calls(), 0);
}

#[test]
fn deadline_expiring_after_opening_observation_prevents_further_work() {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(1);
    let observations = Cell::new(0usize);
    struct CountingObserver<'a> {
        inner: FixtureObserver,
        observations: &'a Cell<usize>,
    }
    impl super::super::super::gpt_partition_device::BlockDeviceObserver for CountingObserver<'_> {
        fn observe_until(
            &mut self,
            deadline: Instant,
        ) -> io::Result<super::super::super::gpt_partition_device::BlockDeviceObservation> {
            let observation = super::super::super::gpt_partition_device::BlockDeviceObserver::observe_until(
                &mut self.inner,
                deadline,
            )?;
            self.observations.set(self.observations.get() + 1);
            Ok(observation)
        }
    }
    let mut observer = CountingObserver {
        inner: FixtureObserver::stable(ObservationFields::standard()),
        observations: &observations,
    };
    let mut clock = || {
        if observations.get() == 0 {
            start
        } else {
            deadline + Duration::from_nanos(1)
        }
    };

    let error = super::super::super::gpt_partition_device::reconcile_gpt_partition_device_fixture_with_clock_until(
        &mut observer,
        8,
        0,
        1,
        super::support::UUID,
        2_048,
        4_096,
        super::super::super::gpt_partition_role::GptPartitionRole::Esp,
        1,
        super::support::UUID,
        2_048,
        4_096,
        512,
        64 * 1024 * 1024,
        super::support::TABLE_HASH,
        PRODUCTION_LIMITS,
        deadline,
        Some(&mut clock),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(observations.get(), 1);
    assert_eq!(observer.inner.calls(), 1);
}

#[test]
fn deadline_equality_is_admitted_by_the_injected_clock() {
    let deadline = Instant::now();
    let mut observer = FixtureObserver::stable(ObservationFields::standard());
    let mut clock = || deadline;
    authenticate_with_clock(
        &mut observer,
        FixtureInput::standard(),
        PRODUCTION_LIMITS,
        deadline,
        &mut clock,
    )
    .unwrap();
    assert_eq!(observer.calls(), 2);
}
