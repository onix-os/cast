//! Bounded database evidence for an ActiveReblit boot projection.

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use stone::StonePayloadLayoutRecord;

use crate::{State, db, package, state};

const MAX_PROJECTION_PACKAGES: usize = 4_096;
const MAX_PROJECTION_PACKAGE_ID_BYTES: usize = 1024 * 1024;
const MAX_PROJECTION_LAYOUT_ROWS: usize = 262_144;
const MAX_PROJECTION_LAYOUT_STRING_BYTES: usize = 64 * 1024 * 1024;
const PROJECTION_DATABASE_TIMEOUT: Duration = Duration::from_secs(120);

const PROJECTION_POLICY: ProjectionPolicy = ProjectionPolicy {
    max_packages: MAX_PROJECTION_PACKAGES,
    max_package_id_bytes: MAX_PROJECTION_PACKAGE_ID_BYTES,
    layout_bounds: db::layout::QueryBounds {
        max_rows: MAX_PROJECTION_LAYOUT_ROWS,
        max_string_bytes: MAX_PROJECTION_LAYOUT_STRING_BYTES,
    },
    timeout: PROJECTION_DATABASE_TIMEOUT,
};

/// A frozen state-and-layout database projection prepared before boot repair.
///
/// This value is intentionally not `Clone`: later phases must retain and
/// revalidate the one authority-bearing projection rather than duplicate it.
pub(crate) struct PreparedActiveReblitBootProjection {
    states: db::state::FrozenBootInput,
    layouts: Vec<(package::Id, StonePayloadLayoutRecord)>,
}

impl PreparedActiveReblitBootProjection {
    /// Capture the active head, bounded history, and their canonical package
    /// layouts with a state-layout-layout-state sandwich.
    #[cfg(test)]
    pub(crate) fn prepare(
        state_db: &db::state::Database,
        layout_db: &db::layout::Database,
        head: state::Id,
    ) -> Result<Self, ActiveReblitBootProjectionError> {
        let deadline = projection_deadline(PROJECTION_POLICY.timeout)?;
        Self::prepare_until(state_db, layout_db, head, deadline)
    }

    /// Capture without replacing the caller-owned absolute deadline.
    pub(crate) fn prepare_until(
        state_db: &db::state::Database,
        layout_db: &db::layout::Database,
        head: state::Id,
        deadline: Instant,
    ) -> Result<Self, ActiveReblitBootProjectionError> {
        capture_database_projection_until(state_db, layout_db, head, PROJECTION_POLICY, deadline)
    }

    pub(crate) fn head(&self) -> &State {
        self.states.head()
    }

    pub(crate) fn states(&self) -> &[State] {
        self.states.states()
    }

    pub(crate) fn layouts(&self) -> &[(package::Id, StonePayloadLayoutRecord)] {
        &self.layouts
    }

    /// Repeat both bounded database reads and require exact equality with the
    /// originally prepared state and layout evidence.
    #[cfg(test)]
    pub(crate) fn revalidate(
        &self,
        state_db: &db::state::Database,
        layout_db: &db::layout::Database,
    ) -> Result<(), ActiveReblitBootProjectionError> {
        let deadline = projection_deadline(PROJECTION_POLICY.timeout)?;
        self.revalidate_until(state_db, layout_db, deadline)
    }

    /// Revalidate without replacing the caller-owned absolute deadline.
    pub(crate) fn revalidate_until(
        &self,
        state_db: &db::state::Database,
        layout_db: &db::layout::Database,
        deadline: Instant,
    ) -> Result<(), ActiveReblitBootProjectionError> {
        let mut now = Instant::now;
        self.revalidate_until_with_clock(state_db, layout_db, deadline, &mut now)
    }

    fn revalidate_until_with_clock<N>(
        &self,
        state_db: &db::state::Database,
        layout_db: &db::layout::Database,
        deadline: Instant,
        now: &mut N,
    ) -> Result<(), ActiveReblitBootProjectionError>
    where
        N: FnMut() -> Instant,
    {
        let current = capture_database_projection_until_with_clock(
            state_db,
            layout_db,
            self.head().id,
            PROJECTION_POLICY,
            deadline,
            now,
        )?;

        let states_match = current.states == self.states;
        require_before_deadline_with_clock(deadline, PROJECTION_POLICY.timeout, now)?;
        if !states_match {
            return Err(ActiveReblitBootProjectionError::StateChanged);
        }
        let layouts_match = current.layouts == self.layouts;
        require_before_deadline_with_clock(deadline, PROJECTION_POLICY.timeout, now)?;
        if !layouts_match {
            return Err(ActiveReblitBootProjectionError::LayoutChanged);
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ProjectionPolicy {
    max_packages: usize,
    max_package_id_bytes: usize,
    layout_bounds: db::layout::QueryBounds,
    timeout: Duration,
}

struct ProjectionSnapshot {
    states: db::state::FrozenBootInput,
    layouts: Vec<(package::Id, StonePayloadLayoutRecord)>,
}

fn capture_database_projection_until(
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
    head: state::Id,
    policy: ProjectionPolicy,
    deadline: Instant,
) -> Result<PreparedActiveReblitBootProjection, ActiveReblitBootProjectionError> {
    let mut now = Instant::now;
    capture_database_projection_until_with_clock(state_db, layout_db, head, policy, deadline, &mut now)
}

fn capture_database_projection_until_with_clock<N>(
    state_db: &db::state::Database,
    layout_db: &db::layout::Database,
    head: state::Id,
    policy: ProjectionPolicy,
    deadline: Instant,
    now: &mut N,
) -> Result<PreparedActiveReblitBootProjection, ActiveReblitBootProjectionError>
where
    N: FnMut() -> Instant,
{
    let snapshot =
        capture_with_layout_query_and_clock(state_db, head, policy, deadline, now, |packages, bounds, deadline| {
            layout_db
                .query_bounded(packages, bounds, || Instant::now() <= deadline)
                .map_err(ActiveReblitBootProjectionError::LayoutDatabase)
        })?;

    Ok(PreparedActiveReblitBootProjection {
        states: snapshot.states,
        layouts: snapshot.layouts,
    })
}

#[cfg(test)]
fn capture_with_layout_query<F>(
    state_db: &db::state::Database,
    head: state::Id,
    policy: ProjectionPolicy,
    deadline: Instant,
    query_layouts: F,
) -> Result<ProjectionSnapshot, ActiveReblitBootProjectionError>
where
    F: FnMut(
        &[package::Id],
        db::layout::QueryBounds,
        Instant,
    ) -> Result<db::layout::BoundedQueryOutcome, ActiveReblitBootProjectionError>,
{
    let mut now = Instant::now;
    capture_with_layout_query_and_clock(state_db, head, policy, deadline, &mut now, query_layouts)
}

fn capture_with_layout_query_and_clock<F, N>(
    state_db: &db::state::Database,
    head: state::Id,
    policy: ProjectionPolicy,
    deadline: Instant,
    now: &mut N,
    mut query_layouts: F,
) -> Result<ProjectionSnapshot, ActiveReblitBootProjectionError>
where
    F: FnMut(
        &[package::Id],
        db::layout::QueryBounds,
        Instant,
    ) -> Result<db::layout::BoundedQueryOutcome, ActiveReblitBootProjectionError>,
    N: FnMut() -> Instant,
{
    require_before_deadline_with_clock(deadline, policy.timeout, now)?;
    let states_before = state_db.frozen_boot_input(head)?;
    require_before_deadline_with_clock(deadline, policy.timeout, now)?;

    let packages = canonical_selected_packages(&states_before, policy, deadline, now)?;
    require_before_deadline_with_clock(deadline, policy.timeout, now)?;

    let layouts_before = complete_layout_query(
        query_layouts(&packages, policy.layout_bounds, deadline)?,
        policy.timeout,
    )?;
    require_before_deadline_with_clock(deadline, policy.timeout, now)?;
    let layouts_after = complete_layout_query(
        query_layouts(&packages, policy.layout_bounds, deadline)?,
        policy.timeout,
    )?;
    require_before_deadline_with_clock(deadline, policy.timeout, now)?;
    let layouts_match = layouts_before == layouts_after;
    require_before_deadline_with_clock(deadline, policy.timeout, now)?;
    if !layouts_match {
        return Err(ActiveReblitBootProjectionError::LayoutSandwichChanged);
    }

    let states_after = state_db.frozen_boot_input(head)?;
    require_before_deadline_with_clock(deadline, policy.timeout, now)?;
    let states_match = states_before == states_after;
    require_before_deadline_with_clock(deadline, policy.timeout, now)?;
    if !states_match {
        return Err(ActiveReblitBootProjectionError::StateSandwichChanged);
    }

    // Equality and complete output materialization are part of the bounded
    // capture, not work that may escape after the caller's deadline.
    let snapshot = ProjectionSnapshot {
        states: states_before,
        layouts: layouts_before,
    };
    require_before_deadline_with_clock(deadline, policy.timeout, now)?;

    Ok(snapshot)
}

fn complete_layout_query(
    outcome: db::layout::BoundedQueryOutcome,
    timeout: Duration,
) -> Result<Vec<(package::Id, StonePayloadLayoutRecord)>, ActiveReblitBootProjectionError> {
    match outcome {
        db::layout::BoundedQueryOutcome::Complete(layouts) => Ok(layouts),
        db::layout::BoundedQueryOutcome::PackageLimit { limit, actual } => {
            return Err(ActiveReblitBootProjectionError::LayoutPackageCountLimit { limit, actual });
        }
        db::layout::BoundedQueryOutcome::PackageIdByteLimit { limit, actual } => {
            return Err(ActiveReblitBootProjectionError::LayoutPackageIdByteLimit { limit, actual });
        }
        db::layout::BoundedQueryOutcome::RowLimit { limit, actual } => {
            return Err(ActiveReblitBootProjectionError::LayoutRowLimit { limit, actual });
        }
        db::layout::BoundedQueryOutcome::StringByteLimit { limit, actual } => {
            return Err(ActiveReblitBootProjectionError::LayoutStringByteLimit { limit, actual });
        }
        db::layout::BoundedQueryOutcome::Cancelled => {
            return Err(ActiveReblitBootProjectionError::LayoutQueryCancelled { timeout });
        }
    }
}

fn canonical_selected_packages<N>(
    states: &db::state::FrozenBootInput,
    policy: ProjectionPolicy,
    deadline: Instant,
    now: &mut N,
) -> Result<Vec<package::Id>, ActiveReblitBootProjectionError>
where
    N: FnMut() -> Instant,
{
    let mut packages = BTreeSet::new();
    let mut package_id_bytes = 0usize;

    for selection in states.states().iter().flat_map(|state| &state.selections) {
        require_before_deadline_with_clock(deadline, policy.timeout, now)?;
        if packages.contains(&selection.package) {
            continue;
        }

        let actual_packages = packages.len().saturating_add(1);
        if actual_packages > policy.max_packages {
            return Err(ActiveReblitBootProjectionError::PackageCountLimit {
                limit: policy.max_packages,
                actual: actual_packages,
            });
        }

        package_id_bytes = package_id_bytes
            .checked_add(selection.package.as_str().len())
            .unwrap_or(usize::MAX);
        if package_id_bytes > policy.max_package_id_bytes {
            return Err(ActiveReblitBootProjectionError::PackageIdByteLimit {
                limit: policy.max_package_id_bytes,
                actual: package_id_bytes,
            });
        }

        packages.insert(selection.package.clone());
    }

    Ok(packages.into_iter().collect())
}

#[cfg(test)]
fn projection_deadline(timeout: Duration) -> Result<Instant, ActiveReblitBootProjectionError> {
    Instant::now()
        .checked_add(timeout)
        .ok_or(ActiveReblitBootProjectionError::InvalidDeadline { timeout })
}

fn require_before_deadline_with_clock<N>(
    deadline: Instant,
    timeout: Duration,
    now: &mut N,
) -> Result<(), ActiveReblitBootProjectionError>
where
    N: FnMut() -> Instant,
{
    if now() > deadline {
        return Err(ActiveReblitBootProjectionError::DeadlineExceeded { timeout });
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ActiveReblitBootProjectionError {
    #[error(transparent)]
    StateDatabase(#[from] db::state::FrozenBootInputError),
    #[error("layout database query failed: {0}")]
    LayoutDatabase(#[source] db::Error),
    #[error("boot projection package count exceeds {limit} (got {actual})")]
    PackageCountLimit { limit: usize, actual: usize },
    #[error("boot projection package IDs exceed {limit} bytes (got {actual})")]
    PackageIdByteLimit { limit: usize, actual: usize },
    #[error("layout query rejected {actual} packages against its limit of {limit}")]
    LayoutPackageCountLimit { limit: usize, actual: usize },
    #[error("layout query rejected {actual} package-ID bytes against its limit of {limit}")]
    LayoutPackageIdByteLimit { limit: usize, actual: usize },
    #[error("boot projection layout rows exceed {limit} (got {actual})")]
    LayoutRowLimit { limit: usize, actual: usize },
    #[error("boot projection layout strings exceed {limit} bytes (got {actual})")]
    LayoutStringByteLimit { limit: usize, actual: usize },
    #[error("boot projection layout query was cancelled within its {timeout:?} deadline policy")]
    LayoutQueryCancelled { timeout: Duration },
    #[error("boot projection deadline could not be represented for timeout {timeout:?}")]
    InvalidDeadline { timeout: Duration },
    #[error("boot projection exceeded its {timeout:?} database deadline")]
    DeadlineExceeded { timeout: Duration },
    #[error("boot projection layouts changed across the two bounded layout reads")]
    LayoutSandwichChanged,
    #[error("boot projection states changed across the state-layout-layout-state sandwich")]
    StateSandwichChanged,
    #[error("boot projection state evidence changed after preparation")]
    StateChanged,
    #[error("boot projection layout evidence changed after preparation")]
    LayoutChanged,
}

#[path = "active_reblit_asset_plan.rs"]
mod asset_plan;
#[allow(unused_imports)] // consumed by the sealed-asset and systemd-plan slices
pub(crate) use asset_plan::{
    ActiveReblitBootAssetPlanError, BootAssetPlanNotApplicable, BootAssetPlanOutcome, BootAssetRole,
    BootSchemaFallback, BootSchemaSource, MAX_BOOT_PLAN_ASSETS, MAX_BOOT_PLAN_SNAPSHOT_DIGESTS, PlannedBootAsset,
    PlannedBootSchemaRequirement, PreparedActiveReblitBootAssetPlan,
};

#[cfg(test)]
#[path = "active_reblit_projection_tests.rs"]
mod tests;
