//! Authenticated machine-local declarative intent for ActiveReblit boot topology.
//!
//! The fixed `etc/cast/boot-topology.glu` program is separate from the stored
//! [`crate::SystemModel`]: it describes one machine's immutable partition
//! identities, not stateless OS package intent. The restricted program must
//! import exactly `cast.boot_topology.v2` and returns either one ESP selector
//! used for both destinations or distinct ESP and XBOOTLDR selectors. Each
//! selector contains a canonical PARTUUID. It also retains an exact, authored
//! mount-point hint without normalization.
//!
//! This module authenticates only declarative intent. In particular,
//! `DistinctXbootldr` is not evidence that either partition is mounted, has the
//! claimed GPT role, or shares a disk with the ESP. A later physical-topology
//! aggregate must treat each mount-point hint as an untrusted lexical selector,
//! bind each PARTUUID to an authenticated mounted major:minor device, and prove
//! the same-disk relationship before granting destination authority. The hints
//! grant no pathname, mount, filesystem, or mutation authority.
//!
//! Preparation retains every fixed pathname component, the exact regular-file
//! inode and bytes, the evaluated value, and the complete Gluon fingerprint.
//! Only a two-pass, same-thread revalidation exposes borrowed semantic views.
//! No path or mount discovery, filesystem mutation, or publication occurs.

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
    DeclarationEvaluationError, DeclarationEvaluator, EvaluationDeadline,
    Evaluation as DeclarationEvaluation, LanguageSpec, Limits, Source, SourceRoot,
};
use gluon_config::{EvaluationIdentity, EvaluationIdentityValidationError};
use thiserror::Error;

use crate::{Installation, installation};

use self::{
    filesystem::{RetainedBootTopologySource, capture_source, revalidate_source},
    gluon::GluonBootTopologyIntentEvaluator,
};

#[path = "active_reblit_boot_topology_intent/filesystem.rs"]
mod filesystem;
#[path = "active_reblit_boot_topology_intent/gluon.rs"]
mod gluon;
#[path = "active_reblit_boot_topology_intent/lua.rs"]
mod lua;

const KIB: usize = 1024;
const MAX_BOOT_TOPOLOGY_SOURCE_BYTES: usize = 64 * KIB;
const MAX_BOOT_TOPOLOGY_WORK: usize = 4_096;
const BOOT_TOPOLOGY_TIMEOUT: Duration = Duration::from_secs(30);

const BOOT_TOPOLOGY_POLICY: BootTopologyIntentPolicy = BootTopologyIntentPolicy {
    max_source_bytes: MAX_BOOT_TOPOLOGY_SOURCE_BYTES,
    max_work: MAX_BOOT_TOPOLOGY_WORK,
    timeout: BOOT_TOPOLOGY_TIMEOUT,
};

/// Non-cloneable retained source, value, and provenance prepared before effects.
pub(in crate::client) struct PreparedActiveReblitBootTopologyIntent {
    source: RetainedBootTopologySource,
    source_text: Box<str>,
    value: ActiveReblitBootTopologyIntentValue,
    fingerprint: EvaluationIdentity,
    #[cfg(test)]
    preparation_work: usize,
}

/// Same-thread semantic view after two complete source revalidation passes.
pub(in crate::client) struct RevalidatedActiveReblitBootTopologyIntent<'a> {
    intent: &'a PreparedActiveReblitBootTopologyIntent,
    _installation: &'a Installation,
    _same_thread: PhantomData<Rc<()>>,
}

/// Machine-local partition identity intent bound to freshly authenticated bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) enum BoundActiveReblitBootTopologyIntent<'a> {
    BootAliasesEsp {
        esp: BoundActiveReblitBootPartitionSelector<'a>,
    },
    /// Declarative intent only; this is not physical GPT or mount-role proof.
    DistinctXbootldr {
        esp: BoundActiveReblitBootPartitionSelector<'a>,
        xbootldr: BoundActiveReblitBootPartitionSelector<'a>,
    },
}

/// Borrowed declarative selector, never pathname or mounted-device authority.
///
/// `mount_point_hint` retains the exact authored UTF-8 bytes. It is only an
/// untrusted lexical selector for a later authenticated attachment aggregate;
/// it neither proves a mount exists nor permits opening or mutating that path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::client) struct BoundActiveReblitBootPartitionSelector<'a> {
    pub(in crate::client) partuuid: &'a str,
    pub(in crate::client) mount_point_hint: &'a str,
}

#[derive(Debug, Eq, PartialEq)]
struct ActiveReblitBootTopologyIntentValue {
    esp: ActiveReblitBootPartitionSelector,
    boot: ActiveReblitBootTopologyTarget,
}

#[derive(Debug, Eq, PartialEq)]
enum ActiveReblitBootTopologyTarget {
    AliasEsp,
    DistinctXbootldr(ActiveReblitBootPartitionSelector),
}

#[derive(Debug, Eq, PartialEq)]
struct ActiveReblitBootPartitionSelector {
    partuuid: Box<str>,
    mount_point_hint: Box<str>,
}

impl ActiveReblitBootPartitionSelector {
    fn bound(&self) -> BoundActiveReblitBootPartitionSelector<'_> {
        BoundActiveReblitBootPartitionSelector {
            partuuid: &self.partuuid,
            mount_point_hint: &self.mount_point_hint,
        }
    }
}

impl ActiveReblitBootTopologyIntentValue {
    fn bound(&self) -> BoundActiveReblitBootTopologyIntent<'_> {
        match &self.boot {
            ActiveReblitBootTopologyTarget::AliasEsp => {
                BoundActiveReblitBootTopologyIntent::BootAliasesEsp { esp: self.esp.bound() }
            }
            ActiveReblitBootTopologyTarget::DistinctXbootldr(xbootldr) => {
                BoundActiveReblitBootTopologyIntent::DistinctXbootldr {
                    esp: self.esp.bound(),
                    xbootldr: xbootldr.bound(),
                }
            }
        }
    }
}

impl PreparedActiveReblitBootTopologyIntent {
    pub(in crate::client) fn prepare(installation: &Installation) -> Result<Self, ActiveReblitBootTopologyIntentError> {
        let deadline = deadline_after(BOOT_TOPOLOGY_TIMEOUT)?;
        Self::prepare_until(installation, deadline)
    }

    /// Prepare without replacing the caller-owned absolute deadline.
    pub(in crate::client) fn prepare_until(
        installation: &Installation,
        deadline: Instant,
    ) -> Result<Self, ActiveReblitBootTopologyIntentError> {
        prepare_with_policy_until_and_checkpoint(installation, BootTopologyIntentPolicy::production(), deadline, |_| {})
    }

    /// Rebind and reevaluate the exact retained source twice before exposing it.
    pub(in crate::client) fn revalidate<'a>(
        &'a self,
        installation: &'a Installation,
    ) -> Result<RevalidatedActiveReblitBootTopologyIntent<'a>, ActiveReblitBootTopologyIntentError> {
        let deadline = deadline_after(BOOT_TOPOLOGY_TIMEOUT)?;
        self.revalidate_until(installation, deadline)
    }

    /// Revalidate without replacing the caller-owned absolute deadline.
    pub(in crate::client) fn revalidate_until<'a>(
        &'a self,
        installation: &'a Installation,
        deadline: Instant,
    ) -> Result<RevalidatedActiveReblitBootTopologyIntent<'a>, ActiveReblitBootTopologyIntentError> {
        let mut budget =
            BootTopologyIntentBudget::new_until(installation, BootTopologyIntentPolicy::production(), deadline)?;
        self.revalidate_with_budget(installation, &mut budget)?;
        Ok(RevalidatedActiveReblitBootTopologyIntent {
            intent: self,
            _installation: installation,
            _same_thread: PhantomData,
        })
    }

    fn revalidate_with_budget(
        &self,
        installation: &Installation,
        budget: &mut BootTopologyIntentBudget,
    ) -> Result<(), ActiveReblitBootTopologyIntentError> {
        self.revalidate_with_budget_and_checkpoints(installation, budget, || {}, || {}, || {})
    }

    fn revalidate_with_budget_and_checkpoints<F, G, H>(
        &self,
        installation: &Installation,
        budget: &mut BootTopologyIntentBudget,
        after_first_evaluation: F,
        between_passes: G,
        before_final_deadline: H,
    ) -> Result<(), ActiveReblitBootTopologyIntentError>
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
        budget: &mut BootTopologyIntentBudget,
        before_terminal_rebind: F,
    ) -> Result<(), ActiveReblitBootTopologyIntentError>
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
            std::str::from_utf8(&bytes).map_err(|source| ActiveReblitBootTopologyIntentError::InvalidUtf8 {
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

    fn require_exact_source(&self, bytes: &[u8]) -> Result<(), ActiveReblitBootTopologyIntentError> {
        if bytes == self.source_text.as_bytes() {
            Ok(())
        } else {
            Err(ActiveReblitBootTopologyIntentError::Changed {
                path: self.source.path().to_owned(),
                reason: "boot-topology source bytes changed",
            })
        }
    }

    fn require_exact_evaluation(
        &self,
        evaluated: &DeclarationEvaluation<
            ActiveReblitBootTopologyIntentValue,
            EvaluationIdentity,
        >,
    ) -> Result<(), ActiveReblitBootTopologyIntentError> {
        if evaluated.value == self.value && evaluated.identity == self.fingerprint {
            Ok(())
        } else {
            Err(ActiveReblitBootTopologyIntentError::Changed {
                path: self.source.path().to_owned(),
                reason: "boot-topology typed value or evaluation fingerprint changed",
            })
        }
    }

    #[cfg(test)]
    fn preparation_work(&self) -> usize {
        self.preparation_work
    }
}

impl RevalidatedActiveReblitBootTopologyIntent<'_> {
    pub(in crate::client) fn topology(&self) -> BoundActiveReblitBootTopologyIntent<'_> {
        self.intent.value.bound()
    }

    pub(in crate::client) fn fingerprint(&self) -> &EvaluationIdentity {
        &self.intent.fingerprint
    }
}

#[derive(Clone, Copy)]
struct BootTopologyIntentPolicy {
    max_source_bytes: usize,
    max_work: usize,
    timeout: Duration,
}

impl BootTopologyIntentPolicy {
    const fn production() -> Self {
        BOOT_TOPOLOGY_POLICY
    }
}

struct BootTopologyIntentBudget {
    policy: BootTopologyIntentPolicy,
    deadline: Instant,
    remaining_at_admission: Duration,
    work: usize,
    source_path: PathBuf,
    #[cfg(test)]
    clock: Option<Box<dyn Fn() -> Instant>>,
}

impl BootTopologyIntentBudget {
    fn new(
        installation: &Installation,
        policy: BootTopologyIntentPolicy,
    ) -> Result<Self, ActiveReblitBootTopologyIntentError> {
        let deadline = deadline_after(policy.timeout)?;
        Self::new_until(installation, policy, deadline)
    }

    fn new_until(
        installation: &Installation,
        policy: BootTopologyIntentPolicy,
        deadline: Instant,
    ) -> Result<Self, ActiveReblitBootTopologyIntentError> {
        Self::new_until_at(installation, policy, deadline, Instant::now())
    }

    fn new_until_at(
        installation: &Installation,
        policy: BootTopologyIntentPolicy,
        deadline: Instant,
        admitted_at: Instant,
    ) -> Result<Self, ActiveReblitBootTopologyIntentError> {
        let budget = Self {
            policy,
            deadline,
            remaining_at_admission: deadline.saturating_duration_since(admitted_at),
            work: 0,
            source_path: boot_topology_intent_path(installation),
            #[cfg(test)]
            clock: None,
        };
        budget.require_deadline_at_time(&budget.source_path, admitted_at)?;
        Ok(budget)
    }

    #[cfg(test)]
    fn new_until_with_clock(
        installation: &Installation,
        policy: BootTopologyIntentPolicy,
        deadline: Instant,
        clock: impl Fn() -> Instant + 'static,
    ) -> Result<Self, ActiveReblitBootTopologyIntentError> {
        let admitted_at = clock();
        let mut budget = Self::new_until_at(installation, policy, deadline, admitted_at)?;
        budget.clock = Some(Box::new(clock));
        Ok(budget)
    }

    fn step(&mut self, path: &Path) -> Result<(), ActiveReblitBootTopologyIntentError> {
        self.require_deadline_at(path)?;
        let actual = self.work.saturating_add(1);
        if actual > self.policy.max_work {
            return Err(ActiveReblitBootTopologyIntentError::WorkLimit {
                path: path.to_owned(),
                limit: self.policy.max_work,
                actual,
            });
        }
        self.work = actual;
        Ok(())
    }

    fn require_deadline(&self) -> Result<(), ActiveReblitBootTopologyIntentError> {
        self.require_deadline_at(&self.source_path)
    }

    fn require_deadline_at(&self, path: &Path) -> Result<(), ActiveReblitBootTopologyIntentError> {
        self.require_deadline_at_time(path, self.now())
    }

    fn require_deadline_at_time(&self, path: &Path, now: Instant) -> Result<(), ActiveReblitBootTopologyIntentError> {
        if now > self.deadline {
            Err(ActiveReblitBootTopologyIntentError::DeadlineExceeded {
                path: path.to_owned(),
                deadline: self.deadline,
                remaining_at_admission: self.remaining_at_admission,
            })
        } else {
            Ok(())
        }
    }

    fn remaining_duration(&self) -> Result<Duration, ActiveReblitBootTopologyIntentError> {
        self.deadline
            .checked_duration_since(self.now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| ActiveReblitBootTopologyIntentError::DeadlineExceeded {
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

fn prepare_with_policy_and_checkpoint<F>(
    installation: &Installation,
    policy: BootTopologyIntentPolicy,
    before_final_revalidation: F,
) -> Result<PreparedActiveReblitBootTopologyIntent, ActiveReblitBootTopologyIntentError>
where
    F: FnOnce(&PreparedActiveReblitBootTopologyIntent),
{
    let deadline = deadline_after(policy.timeout)?;
    prepare_with_policy_until_and_checkpoint(installation, policy, deadline, before_final_revalidation)
}

fn prepare_with_policy_until_and_checkpoint<F>(
    installation: &Installation,
    policy: BootTopologyIntentPolicy,
    deadline: Instant,
    before_final_revalidation: F,
) -> Result<PreparedActiveReblitBootTopologyIntent, ActiveReblitBootTopologyIntentError>
where
    F: FnOnce(&PreparedActiveReblitBootTopologyIntent),
{
    let mut budget = BootTopologyIntentBudget::new_until(installation, policy, deadline)?;
    prepare_with_budget_and_checkpoints(installation, &mut budget, before_final_revalidation, || {})
}

#[cfg(test)]
fn prepare_with_policy_until_and_clock<F, G>(
    installation: &Installation,
    policy: BootTopologyIntentPolicy,
    deadline: Instant,
    before_final_deadline: F,
    clock: G,
) -> Result<PreparedActiveReblitBootTopologyIntent, ActiveReblitBootTopologyIntentError>
where
    F: FnOnce(),
    G: Fn() -> Instant + 'static,
{
    let mut budget = BootTopologyIntentBudget::new_until_with_clock(installation, policy, deadline, clock)?;
    prepare_with_budget_and_checkpoints(installation, &mut budget, |_| {}, before_final_deadline)
}

fn prepare_with_budget_and_checkpoints<F, G>(
    installation: &Installation,
    budget: &mut BootTopologyIntentBudget,
    before_final_revalidation: F,
    before_final_deadline: G,
) -> Result<PreparedActiveReblitBootTopologyIntent, ActiveReblitBootTopologyIntentError>
where
    F: FnOnce(&PreparedActiveReblitBootTopologyIntent),
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
        .map_err(|source| ActiveReblitBootTopologyIntentError::InvalidUtf8 {
            path: boot_topology_intent_path(installation),
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
    let prepared = PreparedActiveReblitBootTopologyIntent {
        source,
        source_text,
        value: evaluated.value,
        fingerprint: evaluated.identity,
        #[cfg(test)]
        preparation_work: 0,
    };
    before_final_revalidation(&prepared);
    prepared.revalidate_with_budget_and_checkpoints(installation, budget, || {}, || {}, before_final_deadline)?;
    #[cfg(test)]
    let prepared = PreparedActiveReblitBootTopologyIntent {
        preparation_work: budget.work,
        ..prepared
    };
    Ok(prepared)
}

/// One registered boot-topology declaration language, selected by the fixed
/// source's extension. Both engines reach the identical validated intent value
/// through the shared assembly; the conversion error type is shared.
enum BootTopologyIntentEvaluator<'budget> {
    Gluon(gluon::GluonBootTopologyIntentEvaluator<'budget>),
    Lua(lua::LuaBootTopologyIntentEvaluator<'budget>),
}

impl DeclarationEvaluator<ActiveReblitBootTopologyIntentValue>
    for BootTopologyIntentEvaluator<'_>
{
    type Identity = EvaluationIdentity;
    type Error = ActiveReblitBootTopologyIntentError;

    fn language_spec(&self) -> &LanguageSpec {
        match self {
            Self::Gluon(evaluator) => DeclarationEvaluator::<
                ActiveReblitBootTopologyIntentValue,
            >::language_spec(evaluator),
            Self::Lua(evaluator) => DeclarationEvaluator::<
                ActiveReblitBootTopologyIntentValue,
            >::language_spec(evaluator),
        }
    }

    fn limits(&self) -> Limits {
        match self {
            Self::Gluon(evaluator) => {
                DeclarationEvaluator::<ActiveReblitBootTopologyIntentValue>::limits(evaluator)
            }
            Self::Lua(evaluator) => {
                DeclarationEvaluator::<ActiveReblitBootTopologyIntentValue>::limits(evaluator)
            }
        }
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        match self {
            Self::Gluon(evaluator) => Self::Gluon(DeclarationEvaluator::<
                ActiveReblitBootTopologyIntentValue,
            >::with_source_root(evaluator, source_root)),
            Self::Lua(evaluator) => Self::Lua(DeclarationEvaluator::<
                ActiveReblitBootTopologyIntentValue,
            >::with_source_root(evaluator, source_root)),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<ActiveReblitBootTopologyIntentValue, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        match self {
            Self::Gluon(evaluator) => evaluator.evaluate_within(source, deadline),
            Self::Lua(evaluator) => evaluator.evaluate_within(source, deadline),
        }
    }
}

fn evaluate_declaration(
    source_text: &str,
    language: &LanguageSpec,
    logical_name: &str,
    budget: &BootTopologyIntentBudget,
) -> Result<
    DeclarationEvaluation<
        ActiveReblitBootTopologyIntentValue,
        EvaluationIdentity,
    >,
    ActiveReblitBootTopologyIntentError,
> {
    let evaluators = TypedDeclarationEvaluatorSet::new([
        BootTopologyIntentEvaluator::Gluon(GluonBootTopologyIntentEvaluator::new(budget)?),
        BootTopologyIntentEvaluator::Lua(lua::LuaBootTopologyIntentEvaluator::new(budget)?),
    ])
    .expect("the boot-topology adapters register distinct extensions");
    let evaluator = evaluators.get(language).ok_or(
        ActiveReblitBootTopologyIntentError::EvaluationContract {
            reason: "boot-topology source language has no registered evaluator",
        },
    )?;
    let source = Source::new(logical_name, source_text);
    evaluator.evaluate(&source).map_err(|error| match error {
        DeclarationEvaluationError::Evaluation(source) => {
            ActiveReblitBootTopologyIntentError::Evaluation(source)
        }
        DeclarationEvaluationError::Conversion(source) => source,
    })
}

fn registered_declaration_languages() -> RegisteredLanguages {
    RegisteredLanguages::new([gluon::language_spec(), lua::language_spec()])
        .expect("the production boot-topology languages register distinct extensions")
}

fn revalidate_installation_root(
    installation: &Installation,
    budget: &mut BootTopologyIntentBudget,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    budget.step(&installation.root)?;
    installation.revalidate_root_directory_until(budget.deadline)?;
    budget.require_deadline_at(&installation.root)
}

fn boot_topology_intent_path(installation: &Installation) -> PathBuf {
    installation.root.join("etc/cast/boot-topology.glu")
}

fn deadline_after(timeout: Duration) -> Result<Instant, ActiveReblitBootTopologyIntentError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or(ActiveReblitBootTopologyIntentError::InvalidDeadline { timeout })
}

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootTopologyIntentError {
    #[error("revalidate the authenticated installation root around local boot-topology intent")]
    Installation(#[from] installation::Error),
    #[error("boot-topology intent deadline {timeout:?} cannot be represented")]
    InvalidDeadline { timeout: Duration },
    #[error(
        "boot-topology intent exceeded caller-owned absolute deadline {deadline:?} at `{}` (remaining at admission {remaining_at_admission:?})",
        path.display()
    )]
    DeadlineExceeded {
        path: PathBuf,
        deadline: Instant,
        remaining_at_admission: Duration,
    },
    #[error("boot-topology intent exceeded its work limit of {limit} at `{}` (actual {actual})", path.display())]
    WorkLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("required machine-local boot-topology intent is missing at `{}`", path.display())]
    Missing { path: PathBuf },
    #[error("boot-topology source exceeds {limit} bytes at `{}` (actual {actual})", path.display())]
    SourceBytesLimit { path: PathBuf, limit: usize, actual: u64 },
    #[error("boot-topology source is not UTF-8 at `{}`", path.display())]
    InvalidUtf8 {
        path: PathBuf,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("invalid {field} PARTUUID {value_preview:?} (actual {actual_bytes} bytes): {reason}")]
    InvalidPartUuid {
        field: &'static str,
        value_preview: Box<str>,
        actual_bytes: usize,
        reason: &'static str,
    },
    #[error("invalid {field} mount-point selector {value_preview:?} (actual {actual_bytes} bytes): {reason}")]
    InvalidMountPointSelector {
        field: &'static str,
        value_preview: Box<str>,
        actual_bytes: usize,
        reason: &'static str,
    },
    #[error("invalid boot-topology evaluation contract: {reason}")]
    EvaluationContract { reason: &'static str },
    #[error(transparent)]
    Evaluation(#[from] gluon_config::Diagnostic),
    #[error(transparent)]
    EvaluationIdentity(#[from] EvaluationIdentityValidationError),
    #[error("unsafe boot-topology intent inode at `{}`: {reason}", path.display())]
    UnsafeInode { path: PathBuf, reason: &'static str },
    #[error("boot-topology intent changed at `{}`: {reason}", path.display())]
    Changed { path: PathBuf, reason: &'static str },
    #[error("{operation} boot-topology intent capability `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
#[path = "active_reblit_boot_topology_intent_tests.rs"]
mod tests;
