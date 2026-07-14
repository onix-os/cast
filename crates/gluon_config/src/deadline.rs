
use std::time::{Duration, Instant};

use crate::{Diagnostic, LimitKind};

/// One monotonic deadline shared by every stage of an evaluation.
///
/// This deliberately stores the start time and duration instead of computing
/// `start + timeout`: arbitrarily large configured durations must not overflow
/// an [`Instant`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct EvaluationDeadline {
    started_at: Instant,
    timeout: Duration,
}

impl EvaluationDeadline {
    pub(crate) fn start(timeout: Duration) -> Self {
        Self {
            started_at: Instant::now(),
            timeout,
        }
    }

    pub(crate) fn check(self, source_name: &str) -> Result<(), Diagnostic> {
        self.remaining(source_name).map(|_| ())
    }

    pub(crate) fn remaining(self, source_name: &str) -> Result<Duration, Diagnostic> {
        self.remaining_duration().ok_or_else(|| self.exceeded(source_name))
    }

    pub(crate) fn expired(self) -> bool {
        self.remaining_duration().is_none()
    }

    pub(crate) fn remaining_duration(self) -> Option<Duration> {
        self.remaining_at(Instant::now())
    }

    pub(crate) fn exceeded(self, source_name: &str) -> Diagnostic {
        Diagnostic::limit(
            LimitKind::Time,
            Some(source_name.to_owned()),
            format!("evaluation exceeded its {:?} deadline", self.timeout),
        )
    }

    fn remaining_at(self, now: Instant) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.started_at);
        self.timeout
            .checked_sub(elapsed)
            .filter(|remaining| !remaining.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DiagnosticCategory;

    #[test]
    fn remaining_time_is_always_measured_from_the_original_start() {
        let started_at = Instant::now();
        let deadline = EvaluationDeadline {
            started_at,
            timeout: Duration::from_millis(10),
        };

        assert_eq!(deadline.remaining_at(started_at), Some(Duration::from_millis(10)));
        assert_eq!(
            deadline.remaining_at(started_at + Duration::from_millis(4)),
            Some(Duration::from_millis(6))
        );
        assert_eq!(deadline.remaining_at(started_at + Duration::from_millis(10)), None);
        assert_eq!(deadline.remaining_at(started_at + Duration::from_millis(11)), None);
    }

    #[test]
    fn zero_timeout_is_immediately_a_structured_time_limit() {
        let started_at = Instant::now();
        let deadline = EvaluationDeadline {
            started_at,
            timeout: Duration::ZERO,
        };

        assert_eq!(deadline.remaining_at(started_at), None);
        let error = deadline.exceeded("root.glu");
        assert_eq!(error.category, DiagnosticCategory::Limit);
        assert_eq!(error.limit, Some(LimitKind::Time));
        assert_eq!(error.source_name.as_deref(), Some("root.glu"));
    }

    #[test]
    fn times_before_the_start_do_not_extend_the_configured_budget() {
        let started_at = Instant::now();
        let deadline = EvaluationDeadline {
            started_at,
            timeout: Duration::from_millis(10),
        };

        assert_eq!(
            deadline.remaining_at(started_at - Duration::from_millis(1)),
            Some(Duration::from_millis(10))
        );
    }
}
