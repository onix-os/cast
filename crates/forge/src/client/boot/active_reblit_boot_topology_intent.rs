//! Authenticated machine-local declarative intent for ActiveReblit boot topology.
//!
//! The fixed `etc/cast/boot-topology.glu` program is separate from the stored
//! [`crate::SystemModel`]: it describes one machine's immutable partition
//! identities, not stateless OS package intent. The restricted program must
//! import exactly `cast.boot_topology.v1` and returns either one ESP PARTUUID
//! used for both destinations or distinct ESP and XBOOTLDR PARTUUIDs.
//!
//! This module authenticates only declarative intent. In particular,
//! `DistinctXbootldr` is not evidence that either partition is mounted, has the
//! claimed GPT role, or shares a disk with the ESP. A later physical-topology
//! aggregate must bind these PARTUUIDs to authenticated mounted major:minor
//! devices and prove the same-disk relationship before granting destination
//! authority.
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

use gluon_config::{EvaluationFingerprint, EvaluationFingerprintValidationError};
use thiserror::Error;

use crate::{Installation, installation};

use self::{
    filesystem::{RetainedBootTopologySource, capture_source, revalidate_source},
    gluon::EvaluatedBootTopologyIntent,
};

#[path = "active_reblit_boot_topology_intent/filesystem.rs"]
mod filesystem;
#[path = "active_reblit_boot_topology_intent/gluon.rs"]
mod gluon;

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
    fingerprint: EvaluationFingerprint,
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
        esp_partuuid: &'a str,
    },
    /// Declarative intent only; this is not physical GPT or mount-role proof.
    DistinctXbootldr {
        esp_partuuid: &'a str,
        xbootldr_partuuid: &'a str,
    },
}

#[derive(Debug, Eq, PartialEq)]
struct ActiveReblitBootTopologyIntentValue {
    esp_partuuid: Box<str>,
    boot: ActiveReblitBootTopologyTarget,
}

#[derive(Debug, Eq, PartialEq)]
enum ActiveReblitBootTopologyTarget {
    AliasEsp,
    DistinctXbootldr(Box<str>),
}

impl ActiveReblitBootTopologyIntentValue {
    fn bound(&self) -> BoundActiveReblitBootTopologyIntent<'_> {
        match &self.boot {
            ActiveReblitBootTopologyTarget::AliasEsp => BoundActiveReblitBootTopologyIntent::BootAliasesEsp {
                esp_partuuid: &self.esp_partuuid,
            },
            ActiveReblitBootTopologyTarget::DistinctXbootldr(xbootldr_partuuid) => {
                BoundActiveReblitBootTopologyIntent::DistinctXbootldr {
                    esp_partuuid: &self.esp_partuuid,
                    xbootldr_partuuid,
                }
            }
        }
    }
}

impl PreparedActiveReblitBootTopologyIntent {
    pub(in crate::client) fn prepare(installation: &Installation) -> Result<Self, ActiveReblitBootTopologyIntentError> {
        prepare_with_policy_and_checkpoint(installation, BootTopologyIntentPolicy::production(), |_| {})
    }

    /// Rebind and reevaluate the exact retained source twice before exposing it.
    pub(in crate::client) fn revalidate<'a>(
        &'a self,
        installation: &'a Installation,
    ) -> Result<RevalidatedActiveReblitBootTopologyIntent<'a>, ActiveReblitBootTopologyIntentError> {
        let mut budget = BootTopologyIntentBudget::new(installation, BootTopologyIntentPolicy::production())?;
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
        self.revalidate_with_budget_and_checkpoints(installation, budget, || {}, || {})
    }

    fn revalidate_with_budget_and_checkpoints<F, G>(
        &self,
        installation: &Installation,
        budget: &mut BootTopologyIntentBudget,
        after_first_evaluation: F,
        between_passes: G,
    ) -> Result<(), ActiveReblitBootTopologyIntentError>
    where
        F: FnOnce(),
        G: FnOnce(),
    {
        revalidate_installation_root(installation, budget)?;
        self.revalidate_complete_pass(installation, budget, after_first_evaluation)?;
        revalidate_installation_root(installation, budget)?;
        between_passes();
        revalidate_installation_root(installation, budget)?;
        self.revalidate_complete_pass(installation, budget, || {})?;
        revalidate_installation_root(installation, budget)?;
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
        let bytes = revalidate_source(installation, &self.source, budget)?;
        self.require_exact_source(&bytes)?;
        let source_text =
            std::str::from_utf8(&bytes).map_err(|source| ActiveReblitBootTopologyIntentError::InvalidUtf8 {
                path: budget.source_path.clone(),
                source,
            })?;
        let evaluated = gluon::evaluate(source_text, budget)?;
        self.require_exact_evaluation(&evaluated)?;

        before_terminal_rebind();
        let terminal = revalidate_source(installation, &self.source, budget)?;
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
        evaluated: &EvaluatedBootTopologyIntent,
    ) -> Result<(), ActiveReblitBootTopologyIntentError> {
        if evaluated.value == self.value && evaluated.fingerprint == self.fingerprint {
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

    pub(in crate::client) fn fingerprint(&self) -> &EvaluationFingerprint {
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
    work: usize,
    source_path: PathBuf,
}

impl BootTopologyIntentBudget {
    fn new(
        installation: &Installation,
        policy: BootTopologyIntentPolicy,
    ) -> Result<Self, ActiveReblitBootTopologyIntentError> {
        let deadline =
            Instant::now()
                .checked_add(policy.timeout)
                .ok_or(ActiveReblitBootTopologyIntentError::InvalidDeadline {
                    timeout: policy.timeout,
                })?;
        Ok(Self {
            policy,
            deadline,
            work: 0,
            source_path: boot_topology_intent_path(installation),
        })
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
        if Instant::now() > self.deadline {
            Err(ActiveReblitBootTopologyIntentError::DeadlineExceeded {
                path: path.to_owned(),
                timeout: self.policy.timeout,
            })
        } else {
            Ok(())
        }
    }

    fn remaining_duration(&self) -> Result<Duration, ActiveReblitBootTopologyIntentError> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| ActiveReblitBootTopologyIntentError::DeadlineExceeded {
                path: self.source_path.clone(),
                timeout: self.policy.timeout,
            })
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
    let mut budget = BootTopologyIntentBudget::new(installation, policy)?;
    revalidate_installation_root(installation, &mut budget)?;
    let (source, bytes) = capture_source(installation, &mut budget)?;
    let source_text = std::str::from_utf8(&bytes)
        .map_err(|source| ActiveReblitBootTopologyIntentError::InvalidUtf8 {
            path: boot_topology_intent_path(installation),
            source,
        })?
        .to_owned()
        .into_boxed_str();
    let evaluated = gluon::evaluate(&source_text, &budget)?;
    let prepared = PreparedActiveReblitBootTopologyIntent {
        source,
        source_text,
        value: evaluated.value,
        fingerprint: evaluated.fingerprint,
        #[cfg(test)]
        preparation_work: 0,
    };
    before_final_revalidation(&prepared);
    prepared.revalidate_with_budget(installation, &mut budget)?;
    #[cfg(test)]
    let prepared = PreparedActiveReblitBootTopologyIntent {
        preparation_work: budget.work,
        ..prepared
    };
    Ok(prepared)
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

#[derive(Debug, Error)]
pub(in crate::client) enum ActiveReblitBootTopologyIntentError {
    #[error("revalidate the authenticated installation root around local boot-topology intent")]
    Installation(#[from] installation::Error),
    #[error("boot-topology intent deadline {timeout:?} cannot be represented")]
    InvalidDeadline { timeout: Duration },
    #[error("boot-topology intent exceeded its {timeout:?} deadline at `{}`", path.display())]
    DeadlineExceeded { path: PathBuf, timeout: Duration },
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
    #[error("invalid boot-topology evaluation contract: {reason}")]
    EvaluationContract { reason: &'static str },
    #[error(transparent)]
    Evaluation(#[from] gluon_config::Diagnostic),
    #[error(transparent)]
    EvaluationFingerprint(#[from] EvaluationFingerprintValidationError),
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
