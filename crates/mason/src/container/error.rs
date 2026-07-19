use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Container(#[from] container::Error),
    #[error("read the bounded current-process cgroup membership from {path:?}")]
    ReadCurrentCgroup {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("current-process cgroup membership at {path:?} exceeds the {limit}-byte ceiling")]
    CurrentCgroupTooLarge { path: PathBuf, limit: usize },
    #[error("current unified cgroup path exceeds the {limit}-byte ceiling (got {actual})")]
    CurrentCgroupPathTooLarge { limit: usize, actual: usize },
    #[error("current unified cgroup path exceeds the {limit}-component ceiling (got {actual})")]
    CurrentCgroupComponentLimit { limit: usize, actual: usize },
    #[error("current unified cgroup component exceeds the {limit}-byte ceiling (got {actual})")]
    CurrentCgroupComponentTooLarge { limit: usize, actual: usize },
    #[error("malformed current-process cgroup membership: {reason}")]
    MalformedCurrentCgroup { reason: &'static str },
    #[error(
        "frozen execution requires an explicit systemd delegation whose current cgroup ends in /cast-supervisor; found {current:?}"
    )]
    FrozenCgroupDelegationRequired { current: PathBuf },
    #[error("open and authenticate the explicitly delegated cgroup-v2 root")]
    OpenDelegatedCgroup(#[source] container::cgroup::CgroupError),
    #[error("the frozen derivation ID is not canonical lowercase SHA-256")]
    InvalidDerivationCgroupIdentity,
    #[error("frozen cgroup policy requires a nonzero execution.jobs value")]
    InvalidFrozenCgroupJobs,
    #[error("frozen cgroup limit arithmetic overflowed for {field}")]
    FrozenCgroupLimitOverflow { field: &'static str },
    #[error("construct the finite frozen-derivation cgroup policy")]
    InvalidFrozenCgroupLimits(#[source] container::cgroup::CgroupError),
    #[error("create and configure the authenticated derivation cgroup")]
    CreateDerivationCgroup(#[source] container::cgroup::CgroupError),
    #[error("revalidate the frozen root immediately before container activation")]
    FrozenRoot(#[from] forge::client::Error),
    #[error("open the authenticated frozen-root anchor for container activation")]
    AnchorFrozenRoot(#[source] io::Error),
    #[error("authenticate the frozen root as a child-namespace locator")]
    FrozenRootLocator(#[source] container::AnchoredLocatorError),
    #[error("prepare frozen mount")]
    Mount(#[from] io::Error),
    #[error("frozen derivation layout does not match runtime paths")]
    FrozenLayoutMismatch,
    #[error("frozen execution requires credential policy `isolated-root`, found `{found}`")]
    FrozenCredentialPolicyMismatch { found: &'static str },
    #[error("frozen execution forbids network-enabled sandbox policy")]
    FrozenNetworkPolicyMismatch,
    #[error("prepared frozen root does not match runtime path: expected {expected:?}, found {found:?}")]
    FrozenRootMismatch { expected: PathBuf, found: PathBuf },
    #[error("runtime paths are not bound to the frozen derivation")]
    InvalidFrozenPaths(#[source] io::Error),
    #[error("open the retained frozen workspace {path:?} without following links")]
    OpenFrozenWorkspace {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the retained frozen workspace is not privately controlled: {path:?} (uid={owner}, mode={mode:#06o})")]
    UnsafeFrozenWorkspace { path: PathBuf, owner: u32, mode: u32 },
    #[error("the retained frozen workspace pathname was replaced: {0:?}")]
    FrozenWorkspaceReplaced(PathBuf),
    #[error("invalid frozen external bind source beneath the retained workspace: {0:?}")]
    InvalidFrozenBindSource(PathBuf),
    #[error("prepare frozen external bind source {path:?} without following links or crossing mounts")]
    PrepareFrozenBindSource {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "frozen external bind source is not an exact owner-private directory: {path:?} (uid={owner}, mode={mode:#06o})"
    )]
    UnsafeFrozenBindSource { path: PathBuf, owner: u32, mode: u32 },
    #[error("frozen external bind source crosses a mount beneath the retained workspace: {0:?}")]
    FrozenBindSourceCrossesMount(PathBuf),
    #[error("frozen external bind source pathname was replaced after pinning: {0:?}")]
    FrozenBindSourceReplaced(PathBuf),
    #[error("authenticate frozen external bind source {path:?} as a child-namespace locator")]
    FrozenBindSourceLocator {
        path: PathBuf,
        #[source]
        source: container::AnchoredLocatorError,
    },
    #[error("prepared frozen sandbox has no pinned artefact mount")]
    MissingFrozenArtefactMount,
    #[error("open the materialized frozen mount root {path:?} without following links")]
    OpenFrozenMountRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the materialized frozen mount root is not a privately controlled directory: {0:?}")]
    UnsafeFrozenMountRoot(PathBuf),
    #[error("the materialized frozen mount root was replaced during target preparation: {0:?}")]
    FrozenMountRootReplaced(PathBuf),
    #[error("invalid frozen mount target: {0:?}")]
    InvalidFrozenMountTarget(PathBuf),
    #[error("frozen mount target count exceeds {limit} (got {actual})")]
    FrozenMountTargetLimit { limit: usize, actual: usize },
    #[error("frozen mount targets overlap: {first:?} and {second:?}")]
    OverlappingFrozenMountTargets { first: PathBuf, second: PathBuf },
    #[error("prepare frozen mount target {path:?} without following links or crossing mounts")]
    PrepareFrozenMountTarget {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen mount target must be empty before activation: {0:?}")]
    FrozenMountTargetNotEmpty(PathBuf),
    #[cfg(feature = "delegated-fixture-test-support")]
    #[error("create the private execution-capability preflight root")]
    CreateExecutionPreflightRoot(#[source] io::Error),
    #[cfg(feature = "delegated-fixture-test-support")]
    #[error("open the execution-capability preflight root {path:?} as an O_PATH directory")]
    OpenExecutionPreflightRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[cfg(feature = "delegated-fixture-test-support")]
    #[error("locate the execution-capability preflight root")]
    LocateExecutionPreflightRoot(#[source] container::AnchoredLocatorError),
    #[cfg(feature = "delegated-fixture-test-support")]
    #[error("anchor the execution-capability preflight root")]
    AnchorExecutionPreflightRoot(#[source] io::Error),
    #[cfg(feature = "delegated-fixture-test-support")]
    #[error("prepare execution-capability preflight root path {path:?}")]
    PrepareExecutionPreflightRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[cfg(feature = "delegated-fixture-test-support")]
    #[error("create the private execution-capability preflight bind source")]
    CreateExecutionPreflightBindSource(#[source] io::Error),
    #[cfg(feature = "delegated-fixture-test-support")]
    #[error("open the execution-capability preflight bind source {path:?} as an O_PATH directory")]
    OpenExecutionPreflightBindSource {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[cfg(feature = "delegated-fixture-test-support")]
    #[error("authenticate the execution-capability preflight bind source")]
    AnchorExecutionPreflightBindSource(#[source] container::AnchoredLocatorError),
    #[cfg(feature = "delegated-fixture-test-support")]
    #[error("read execution-capability preflight payload witness {path:?}")]
    VerifyExecutionPreflightPayload {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[cfg(feature = "delegated-fixture-test-support")]
    #[error("execution-capability preflight payload wrote unexpected bytes to {path:?}")]
    UnexpectedExecutionPreflightPayload { path: PathBuf },
}
