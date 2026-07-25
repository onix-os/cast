use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread,
};

use declarative_config::{Diagnostic, EvaluationDeadline, Source, TypedDecoder};
use gluon::RootedThread;

const WATCHDOG_RUNNING: u8 = 0;
const WATCHDOG_COMPLETED: u8 = 1;
const WATCHDOG_TIMED_OUT: u8 = 2;

fn claim_watchdog_terminal_state(state: &AtomicU8, terminal_state: u8) -> bool {
    debug_assert!(matches!(
        terminal_state,
        WATCHDOG_COMPLETED | WATCHDOG_TIMED_OUT
    ));
    state
        .compare_exchange(
            WATCHDOG_RUNNING,
            terminal_state,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

pub(crate) fn run_until_deadline<D>(
    runtime: RootedThread,
    source: &Source,
    deadline: EvaluationDeadline,
    decoder: D,
) -> Result<D::Output, Diagnostic>
where
    D: TypedDecoder<RootedThread>,
{
    let source_name = source.logical_name();
    deadline.check(source_name)?;
    let state = Arc::new(AtomicU8::new(WATCHDOG_RUNNING));
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let watchdog_runtime = runtime.clone();
    let watchdog_state = Arc::clone(&state);
    let watchdog = thread::Builder::new()
        .name("gluon-config-watchdog".to_owned())
        .spawn(move || {
            let timed_out = match deadline.remaining_duration() {
                Some(remaining) => {
                    matches!(
                        done_rx.recv_timeout(remaining),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    )
                }
                None => true,
            };
            if timed_out
                && claim_watchdog_terminal_state(&watchdog_state, WATCHDOG_TIMED_OUT)
            {
                watchdog_runtime.interrupt();
            }
        });
    // Thread creation is part of the same budget, and a late spawn error must
    // not replace the configured time-limit diagnostic.
    let completed_spawn_in_time = !deadline.expired();
    let watchdog = match watchdog {
        Ok(watchdog) if completed_spawn_in_time => watchdog,
        Ok(watchdog) => {
            claim_watchdog_terminal_state(&state, WATCHDOG_TIMED_OUT);
            drop(done_tx);
            watchdog
                .join()
                .map_err(|_| Diagnostic::internal("evaluation watchdog panicked"))?;
            return Err(deadline.exceeded(source_name));
        }
        Err(_) if !completed_spawn_in_time => return Err(deadline.exceeded(source_name)),
        Err(error) => {
            return Err(Diagnostic::internal(format!(
                "start evaluation watchdog: {error}"
            )));
        }
    };

    let result = catch_unwind(AssertUnwindSafe(|| {
        decoder.decode(&runtime, source, deadline)
    }));
    // Completion versus timeout is one atomic race. Decoder cleanup latency
    // cannot relabel a completed evaluation, and a watchdog which already won
    // cannot be overwritten by a late result.
    let completed_state = if deadline.expired() {
        WATCHDOG_TIMED_OUT
    } else {
        WATCHDOG_COMPLETED
    };
    claim_watchdog_terminal_state(&state, completed_state);
    let _ = done_tx.send(());
    let watchdog_result = watchdog.join();
    let completed_state = state.load(Ordering::Acquire);

    if completed_state == WATCHDOG_TIMED_OUT {
        return Err(deadline.exceeded(source_name));
    }
    watchdog_result.map_err(|_| Diagnostic::internal("evaluation watchdog panicked"))?;
    if completed_state != WATCHDOG_COMPLETED {
        return Err(Diagnostic::internal(
            "evaluation watchdog ended without a terminal state",
        ));
    }

    match result {
        Ok(result) => result,
        Err(_) => Err(Diagnostic::internal(format!(
            "Gluon panicked while evaluating {source_name}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_watchdog_terminal_state_wins_without_being_overwritten() {
        let completed_first = AtomicU8::new(WATCHDOG_RUNNING);
        assert!(claim_watchdog_terminal_state(
            &completed_first,
            WATCHDOG_COMPLETED
        ));
        assert!(!claim_watchdog_terminal_state(
            &completed_first,
            WATCHDOG_TIMED_OUT
        ));
        assert_eq!(
            completed_first.load(Ordering::Acquire),
            WATCHDOG_COMPLETED
        );

        let timeout_first = AtomicU8::new(WATCHDOG_RUNNING);
        assert!(claim_watchdog_terminal_state(
            &timeout_first,
            WATCHDOG_TIMED_OUT
        ));
        assert!(!claim_watchdog_terminal_state(
            &timeout_first,
            WATCHDOG_COMPLETED
        ));
        assert_eq!(
            timeout_first.load(Ordering::Acquire),
            WATCHDOG_TIMED_OUT
        );
    }
}
