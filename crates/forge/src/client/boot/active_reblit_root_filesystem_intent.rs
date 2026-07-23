//! Authenticated machine-local root-filesystem intent for ActiveReblit.
//!
//! The mandatory `etc/cast/root-filesystem.glu` source declares one opaque
//! kernel root locator. Rust validates that locator and materializes exactly
//! one `root=<value>` token. The value is deliberately separate from stored
//! OS state, package and administrator snippets, and ESP/XBOOTLDR topology.
//! It proves explicit authored intent only: it neither discovers nor proves a
//! device, filesystem, mount, or boot destination.

use std::{
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use config::declaration::{
    RegisteredLanguages, TypedDeclarationEvaluatorSet,
};
use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator,
    Evaluation as DeclarationEvaluation, LanguageSpec, Source,
};
use gluon_config::{EvaluationFingerprint, EvaluationFingerprintValidationError};
use thiserror::Error;

use crate::{Installation, installation};

use self::{
    filesystem::{RetainedRootFilesystemSource, capture_source, revalidate_source},
    gluon::GluonRootFilesystemIntentEvaluator,
};

#[path = "active_reblit_root_filesystem_intent/filesystem.rs"]
mod filesystem;
#[path = "active_reblit_root_filesystem_intent/gluon.rs"]
mod gluon;
#[path = "active_reblit_root_filesystem_intent/normalization.rs"]
mod normalization;

const KIB: usize = 1024;
const MAX_ROOT_FILESYSTEM_SOURCE_BYTES: usize = 64 * KIB;
const MAX_ROOT_FILESYSTEM_VALUE_BYTES: usize = 4_095;
const MAX_ROOT_FILESYSTEM_WORK: usize = 16_384;
const ROOT_FILESYSTEM_TIMEOUT: Duration = Duration::from_secs(30);

const ROOT_FILESYSTEM_POLICY: RootFilesystemIntentPolicy = RootFilesystemIntentPolicy {
    max_source_bytes: MAX_ROOT_FILESYSTEM_SOURCE_BYTES,
    max_root_bytes: MAX_ROOT_FILESYSTEM_VALUE_BYTES,
    max_work: MAX_ROOT_FILESYSTEM_WORK,
    timeout: ROOT_FILESYSTEM_TIMEOUT,
};

#[derive(Debug, Eq, PartialEq)]
struct RootFilesystemIntentValue {
    root: Box<str>,
    kernel_argument: Box<str>,
}

/// Exact non-cloneable source, evaluated value, and provenance.
pub(in crate::client) struct PreparedActiveReblitRootFilesystemIntent {
    source: RetainedRootFilesystemSource,
    source_text: Box<str>,
    value: RootFilesystemIntentValue,
    fingerprint: EvaluationFingerprint,
    #[cfg(test)]
    preparation_work: usize,
}

/// Same-thread scalar view released only after complete source revalidation.
pub(in crate::client) struct RevalidatedActiveReblitRootFilesystemIntent<'a> {
    intent: &'a PreparedActiveReblitRootFilesystemIntent,
    _installation: &'a Installation,
    _same_thread: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for PreparedActiveReblitRootFilesystemIntent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedActiveReblitRootFilesystemIntent")
            .field("source", &self.source.path())
            .field("kernel_argument", &"retained; revalidation required")
            .field("fingerprint", &self.fingerprint.sha256)
            .finish()
    }
}

impl PreparedActiveReblitRootFilesystemIntent {
    #[cfg(test)]
    pub(in crate::client) fn prepare(
        installation: &Installation,
    ) -> Result<Self, ActiveReblitRootFilesystemIntentError> {
        let deadline = deadline_after(ROOT_FILESYSTEM_TIMEOUT)?;
        Self::prepare_until(installation, deadline)
    }

    /// Prepare without replacing the caller-owned absolute deadline.
    pub(in crate::client) fn prepare_until(
        installation: &Installation,
        deadline: Instant,
    ) -> Result<Self, ActiveReblitRootFilesystemIntentError> {
        prepare_with_policy_until_and_checkpoints(
            installation,
            RootFilesystemIntentPolicy::production(),
            deadline,
            |_| {},
            || {},
        )
    }

    #[cfg(test)]
    pub(in crate::client) fn revalidate<'a>(
        &'a self,
        installation: &'a Installation,
    ) -> Result<RevalidatedActiveReblitRootFilesystemIntent<'a>, ActiveReblitRootFilesystemIntentError> {
        let deadline = deadline_after(ROOT_FILESYSTEM_TIMEOUT)?;
        self.revalidate_until(installation, deadline)
    }

    /// Revalidate without replacing the caller-owned absolute deadline.
    pub(in crate::client) fn revalidate_until<'a>(
        &'a self,
        installation: &'a Installation,
        deadline: Instant,
    ) -> Result<RevalidatedActiveReblitRootFilesystemIntent<'a>, ActiveReblitRootFilesystemIntentError> {
        let mut budget =
            RootFilesystemIntentBudget::new_until(installation, RootFilesystemIntentPolicy::production(), deadline)?;
        self.revalidate_with_budget_and_checkpoints(installation, &mut budget, || {}, || {}, || {})?;
        Ok(RevalidatedActiveReblitRootFilesystemIntent {
            intent: self,
            _installation: installation,
            _same_thread: PhantomData,
        })
    }

    fn revalidate_with_budget_and_checkpoints<F, G, H>(
        &self,
        installation: &Installation,
        budget: &mut RootFilesystemIntentBudget,
        after_first_evaluation: F,
        between_passes: G,
        before_final_deadline: H,
    ) -> Result<(), ActiveReblitRootFilesystemIntentError>
    where
        F: FnOnce(),
        G: FnOnce(),
        H: FnOnce(),
    {
        revalidate_installation_root(installation, budget)?;
        self.revalidate_complete_pass(installation, budget, after_first_evaluation)?;
        revalidate_installation_root(installation, budget)?;
        between_passes();
        revalidate_installation_root(installation, budget)?;
        self.revalidate_complete_pass(installation, budget, || {})?;
        revalidate_installation_root(installation, budget)?;
        before_final_deadline();
        budget.require_deadline()
    }

    fn revalidate_complete_pass<F>(
        &self,
        installation: &Installation,
        budget: &mut RootFilesystemIntentBudget,
        before_terminal_rebind: F,
    ) -> Result<(), ActiveReblitRootFilesystemIntentError>
    where
        F: FnOnce(),
    {
        let languages = registered_declaration_languages();
        let bytes = revalidate_source(
            installation,
            &self.source,
            &languages,
            budget,
        )?;
        self.require_exact_source(&bytes)?;
        let source_text =
            std::str::from_utf8(&bytes).map_err(|source| ActiveReblitRootFilesystemIntentError::InvalidUtf8 {
                path: budget.source_path.clone(),
                source,
            })?;
        let evaluated = evaluate_declaration(
            source_text,
            self.source.language(),
            self.source.logical_name(),
            budget,
        )?;
        self.require_exact_evaluation(&evaluated)?;

        before_terminal_rebind();
        let terminal = revalidate_source(
            installation,
            &self.source,
            &languages,
            budget,
        )?;
        self.require_exact_source(&terminal)
    }

    fn require_exact_source(&self, bytes: &[u8]) -> Result<(), ActiveReblitRootFilesystemIntentError> {
        if bytes == self.source_text.as_bytes() {
            Ok(())
        } else {
            Err(ActiveReblitRootFilesystemIntentError::Changed {
                path: self.source.path().to_owned(),
                reason: "root-filesystem source bytes changed",
            })
        }
    }

    fn require_exact_evaluation(
        &self,
        evaluated: &DeclarationEvaluation<
            RootFilesystemIntentValue,
            EvaluationFingerprint,
        >,
    ) -> Result<(), ActiveReblitRootFilesystemIntentError> {
        if evaluated.value == self.value && evaluated.identity == self.fingerprint {
            Ok(())
        } else {
            Err(ActiveReblitRootFilesystemIntentError::Changed {
                path: self.source.path().to_owned(),
                reason: "root-filesystem typed value or evaluation fingerprint changed",
            })
        }
    }

    #[cfg(test)]
    fn preparation_work(&self) -> usize {
        self.preparation_work
    }
}

impl RevalidatedActiveReblitRootFilesystemIntent<'_> {
    /// The only kernel token supplied by this authority.
    pub(in crate::client) fn kernel_argument(&self) -> &str {
        &self.intent.value.kernel_argument
    }

    pub(in crate::client) fn fingerprint(&self) -> &EvaluationFingerprint {
        &self.intent.fingerprint
    }
}

#[derive(Clone, Copy)]
struct RootFilesystemIntentPolicy {
    max_source_bytes: usize,
    max_root_bytes: usize,
    max_work: usize,
    timeout: Duration,
}

impl RootFilesystemIntentPolicy {
    const fn production() -> Self {
        ROOT_FILESYSTEM_POLICY
    }
}

struct RootFilesystemIntentBudget {
    policy: RootFilesystemIntentPolicy,
    deadline: Instant,
    remaining_at_admission: Duration,
    work: usize,
    source_path: PathBuf,
    #[cfg(test)]
    clock: Option<Box<dyn Fn() -> Instant>>,
}

impl RootFilesystemIntentBudget {
    fn new_until(
        installation: &Installation,
        policy: RootFilesystemIntentPolicy,
        deadline: Instant,
    ) -> Result<Self, ActiveReblitRootFilesystemIntentError> {
        Self::new_until_at(installation, policy, deadline, Instant::now())
    }

    fn new_until_at(
        installation: &Installation,
        policy: RootFilesystemIntentPolicy,
        deadline: Instant,
        admitted_at: Instant,
    ) -> Result<Self, ActiveReblitRootFilesystemIntentError> {
        let budget = Self {
            policy,
            deadline,
            remaining_at_admission: deadline.saturating_duration_since(admitted_at),
            work: 0,
            source_path: root_filesystem_intent_path(installation),
            #[cfg(test)]
            clock: None,
        };
        budget.require_deadline_at_time(&budget.source_path, admitted_at)?;
        Ok(budget)
    }

    #[cfg(test)]
    fn new_until_with_clock(
        installation: &Installation,
        policy: RootFilesystemIntentPolicy,
        deadline: Instant,
        clock: impl Fn() -> Instant + 'static,
    ) -> Result<Self, ActiveReblitRootFilesystemIntentError> {
        let admitted_at = clock();
        let mut budget = Self::new_until_at(installation, policy, deadline, admitted_at)?;
        budget.clock = Some(Box::new(clock));
        Ok(budget)
    }

    fn step(&mut self, checkpoint: &'static str) -> Result<(), ActiveReblitRootFilesystemIntentError> {
        self.reserve_work(1, checkpoint)
    }

    fn reserve_work(
        &mut self,
        amount: usize,
        checkpoint: &'static str,
    ) -> Result<(), ActiveReblitRootFilesystemIntentError> {
        self.require_deadline()?;
        let actual = self.work.checked_add(amount).unwrap_or(usize::MAX);
        if actual > self.policy.max_work {
            return Err(ActiveReblitRootFilesystemIntentError::WorkLimit {
                path: self.source_path.clone(),
                limit: self.policy.max_work,
                actual,
                checkpoint,
            });
        }
        self.work = actual;
        Ok(())
    }

    fn require_deadline(&self) -> Result<(), ActiveReblitRootFilesystemIntentError> {
        self.require_deadline_at(&self.source_path)
    }

    fn require_deadline_at(&self, path: &Path) -> Result<(), ActiveReblitRootFilesystemIntentError> {
        self.require_deadline_at_time(path, self.now())
    }

    fn require_deadline_at_time(&self, path: &Path, now: Instant) -> Result<(), ActiveReblitRootFilesystemIntentError> {
        if now > self.deadline {
            Err(ActiveReblitRootFilesystemIntentError::DeadlineExceeded {
                path: path.to_owned(),
                deadline: self.deadline,
                remaining_at_admission: self.remaining_at_admission,
            })
        } else {
            Ok(())
        }
    }

    fn remaining_duration(&self) -> Result<Duration, ActiveReblitRootFilesystemIntentError> {
        self.deadline
            .checked_duration_since(self.now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| ActiveReblitRootFilesystemIntentError::DeadlineExceeded {
                path: self.source_path.clone(),
                deadline: self.deadline,
                remaining_at_admission: self.remaining_at_admission,
            })
    }

    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(clock) = &self.clock {
            return clock();
        }
        Instant::now()
    }
}

#[cfg(test)]
fn prepare_with_policy_and_checkpoint<F>(
    installation: &Installation,
    policy: RootFilesystemIntentPolicy,
    after_initial_evaluation: F,
) -> Result<PreparedActiveReblitRootFilesystemIntent, ActiveReblitRootFilesystemIntentError>
where
    F: FnOnce(&PreparedActiveReblitRootFilesystemIntent),
{
    let deadline = deadline_after(policy.timeout)?;
    prepare_with_policy_until_and_checkpoints(installation, policy, deadline, after_initial_evaluation, || {})
}

#[cfg(test)]
fn prepare_with_policy_until_and_clock<F, G>(
    installation: &Installation,
    policy: RootFilesystemIntentPolicy,
    deadline: Instant,
    before_final_deadline: F,
    clock: G,
) -> Result<PreparedActiveReblitRootFilesystemIntent, ActiveReblitRootFilesystemIntentError>
where
    F: FnOnce(),
    G: Fn() -> Instant + 'static,
{
    let mut budget = RootFilesystemIntentBudget::new_until_with_clock(installation, policy, deadline, clock)?;
    prepare_with_budget_and_checkpoints(installation, &mut budget, |_| {}, before_final_deadline)
}

fn prepare_with_policy_until_and_checkpoints<F, G>(
    installation: &Installation,
    policy: RootFilesystemIntentPolicy,
    deadline: Instant,
    after_initial_evaluation: F,
    before_final_deadline: G,
) -> Result<PreparedActiveReblitRootFilesystemIntent, ActiveReblitRootFilesystemIntentError>
where
    F: FnOnce(&PreparedActiveReblitRootFilesystemIntent),
    G: FnOnce(),
{
    let mut budget = RootFilesystemIntentBudget::new_until(installation, policy, deadline)?;
    prepare_with_budget_and_checkpoints(
        installation,
        &mut budget,
        after_initial_evaluation,
        before_final_deadline,
    )
}

fn prepare_with_budget_and_checkpoints<F, G>(
    installation: &Installation,
    budget: &mut RootFilesystemIntentBudget,
    after_initial_evaluation: F,
    before_final_deadline: G,
) -> Result<PreparedActiveReblitRootFilesystemIntent, ActiveReblitRootFilesystemIntentError>
where
    F: FnOnce(&PreparedActiveReblitRootFilesystemIntent),
    G: FnOnce(),
{
    revalidate_installation_root(installation, budget)?;
    let languages = registered_declaration_languages();
    let (source, bytes) = capture_source(
        installation,
        &languages,
        budget,
    )?;
    let source_text = std::str::from_utf8(&bytes)
        .map_err(|source| ActiveReblitRootFilesystemIntentError::InvalidUtf8 {
            path: root_filesystem_intent_path(installation),
            source,
        })?
        .to_owned()
        .into_boxed_str();
    let evaluated = evaluate_declaration(
        &source_text,
        source.language(),
        source.logical_name(),
        budget,
    )?;
    let prepared = PreparedActiveReblitRootFilesystemIntent {
        source,
        source_text,
        value: evaluated.value,
        fingerprint: evaluated.identity,
        #[cfg(test)]
        preparation_work: 0,
    };
    after_initial_evaluation(&prepared);
    prepared.revalidate_with_budget_and_checkpoints(installation, budget, || {}, || {}, before_final_deadline)?;
    #[cfg(test)]
    let prepared = PreparedActiveReblitRootFilesystemIntent {
        preparation_work: budget.work,
        ..prepared
    };
    budget.require_deadline()?;
    Ok(prepared)
}

fn evaluate_declaration(
    source_text: &str,
    language: &LanguageSpec,
    logical_name: &str,
    budget: &mut RootFilesystemIntentBudget,
) -> Result<
    DeclarationEvaluation<RootFilesystemIntentValue, EvaluationFingerprint>,
    ActiveReblitRootFilesystemIntentError,
> {
    let evaluator = GluonRootFilesystemIntentEvaluator::new(budget)?;
    let evaluators = TypedDeclarationEvaluatorSet::new([evaluator])
        .expect("one validated root-filesystem adapter has no extension collision");
    let evaluator = evaluators.get(language).ok_or(
        ActiveReblitRootFilesystemIntentError::EvaluationContract {
            reason: "root-filesystem source language has no registered evaluator",
        },
    )?;
    let source = Source::new(logical_name, source_text);
    evaluator.evaluate(&source).map_err(|error| match error {
        DeclarationEvaluationError::Evaluation(source) => {
            ActiveReblitRootFilesystemIntentError::Evaluation(source)
        }
        DeclarationEvaluationError::Conversion(source) => source,
    })
}

fn registered_declaration_languages() -> RegisteredLanguages {
    RegisteredLanguages::new([gluon::language_spec()])
        .expect("the one production root-filesystem language is unique")
}

fn revalidate_installation_root(
    installation: &Installation,
    budget: &mut RootFilesystemIntentBudget,
) -> Result<(), ActiveReblitRootFilesystemIntentError> {
    budget.step("installation root revalidation")?;
    installation.revalidate_root_directory_until(budget.deadline)?;
    budget.require_deadline_at(&installation.root)
}

fn root_filesystem_intent_path(installation: &Installation) -> PathBuf {
    installation.root.join("etc/cast/root-filesystem.glu")
}

fn deadline_after(timeout: Duration) -> Result<Instant, ActiveReblitRootFilesystemIntentError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or(ActiveReblitRootFilesystemIntentError::InvalidDeadline { timeout })
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitRootFilesystemIntentError {
    #[error("revalidate the authenticated installation root around root-filesystem intent")]
    Installation(#[from] installation::Error),
    #[error("root-filesystem intent deadline {timeout:?} cannot be represented")]
    InvalidDeadline { timeout: Duration },
    #[error(
        "root-filesystem intent exceeded caller-owned absolute deadline {deadline:?} at `{}` (remaining at admission {remaining_at_admission:?})",
        path.display()
    )]
    DeadlineExceeded {
        path: PathBuf,
        deadline: Instant,
        remaining_at_admission: Duration,
    },
    #[error("root-filesystem intent work {actual} exceeds limit {limit} at {checkpoint} for `{}`", path.display())]
    WorkLimit {
        path: PathBuf,
        limit: usize,
        actual: usize,
        checkpoint: &'static str,
    },
    #[error("required machine-local root-filesystem intent is missing at `{}`", path.display())]
    Missing { path: PathBuf },
    #[error("root-filesystem source exceeds {limit} bytes at `{}` (actual {actual})", path.display())]
    SourceBytesLimit { path: PathBuf, limit: usize, actual: u64 },
    #[error("root-filesystem source is not UTF-8 at `{}`", path.display())]
    InvalidUtf8 {
        path: PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("root locator exceeds {limit} bytes (actual {actual})")]
    RootBytesLimit { limit: usize, actual: usize },
    #[error("invalid root locator {value_preview:?} (actual {actual_bytes} bytes): {reason}")]
    InvalidRoot {
        value_preview: Box<str>,
        actual_bytes: usize,
        reason: &'static str,
    },
    #[error("invalid root-filesystem evaluation contract: {reason}")]
    EvaluationContract { reason: &'static str },
    #[error(transparent)]
    Evaluation(#[from] gluon_config::Diagnostic),
    #[error(transparent)]
    EvaluationFingerprint(#[from] EvaluationFingerprintValidationError),
    #[error("unsafe root-filesystem intent inode at `{}`: {reason}", path.display())]
    UnsafeInode { path: PathBuf, reason: &'static str },
    #[error("root-filesystem intent changed at `{}`: {reason}", path.display())]
    Changed { path: PathBuf, reason: &'static str },
    #[error("{operation} root-filesystem intent capability `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reserve bounded {resource} while preparing root-filesystem intent")]
    Allocation {
        resource: &'static str,
        #[source]
        source: std::collections::TryReserveError,
    },
}

#[cfg(test)]
#[path = "active_reblit_root_filesystem_intent_tests.rs"]
mod tests;
