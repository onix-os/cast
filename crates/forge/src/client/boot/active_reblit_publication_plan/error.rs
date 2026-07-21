use std::{path::PathBuf, time::Instant};

use thiserror::Error;

use super::{ActiveReblitBootDestinationRoot, ActiveReblitBootPublicationPhase, ActiveReblitBootPublicationRole};

#[derive(Debug, Error, Eq, PartialEq)]
pub(in crate::client) enum ActiveReblitBootPublicationPlanError {
    #[error("boot publication planning exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("boot publication count {actual} exceeds limit {limit}")]
    PublicationCountLimit { limit: usize, actual: usize },
    #[error("boot publication path bytes {actual} exceed limit {limit}")]
    PathByteLimit { limit: usize, actual: usize },
    #[error("boot publication path {path:?} has {actual} bytes, exceeding limit {limit}")]
    SinglePathByteLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("boot publication path {path:?} has {actual} components, exceeding limit {limit}")]
    PathComponentLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("boot publication path is empty")]
    EmptyPath,
    #[error("boot publication path {path:?} is absolute")]
    AbsolutePath { path: PathBuf },
    #[error("boot publication path {path:?} contains an empty component")]
    EmptyPathComponent { path: PathBuf },
    #[error("boot publication path {path:?} contains a dot component")]
    DotPathComponent { path: PathBuf },
    #[error("boot publication path {path:?} contains a parent component")]
    ParentPathComponent { path: PathBuf },
    #[error("boot publication path {path:?} contains NUL")]
    NulPath { path: PathBuf },
    #[error("boot publication path {path:?} is not UTF-8")]
    NonUtf8Path { path: PathBuf },
    #[error("boot publication path {path:?} contains a control character")]
    ControlPathComponent { path: PathBuf },
    #[error("boot publication path {path:?} contains a non-ASCII component")]
    NonAsciiPathComponent { path: PathBuf },
    #[error("boot publication path {path:?} contains reserved private-stage component {component:?}")]
    ReservedPrivatePublicationComponent { path: PathBuf, component: String },
    #[error("boot publication path {path:?} contains a FAT component with {actual} bytes, exceeding limit {limit}")]
    FatComponentByteLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("boot publication path {path:?} contains FAT-forbidden character {character:?}")]
    FatForbiddenCharacter { path: PathBuf, character: char },
    #[error("boot publication path {path:?} contains a component ending in a dot or space")]
    FatTrailingDotOrSpace { path: PathBuf },
    #[error("boot publication path {path:?} contains FAT short-name marker '~'")]
    FatShortNameMarker { path: PathBuf },
    #[error("boot publication path {path:?} contains DOS-reserved component {component:?}")]
    FatReservedName { path: PathBuf, component: String },
    #[error("boot publication role {role:?} requires root {expected:?}, not {actual:?}")]
    RoleRootMismatch {
        role: ActiveReblitBootPublicationRole,
        expected: ActiveReblitBootDestinationRoot,
        actual: ActiveReblitBootDestinationRoot,
    },
    #[error("boot publication role {role:?} requires phase {expected:?}, not {actual:?}")]
    RolePhaseMismatch {
        role: ActiveReblitBootPublicationRole,
        expected: ActiveReblitBootPublicationPhase,
        actual: ActiveReblitBootPublicationPhase,
    },
    #[error("boot publication role {role:?} has the wrong source kind")]
    RoleSourceMismatch { role: ActiveReblitBootPublicationRole },
    #[error("boot publication role {role:?} has invalid destination path {path:?}")]
    RolePathMismatch {
        role: ActiveReblitBootPublicationRole,
        path: PathBuf,
    },
    #[error("sealed boot output {path:?} has {actual} bytes, exceeding limit {limit}")]
    SealedSnapshotFileByteLimit { path: PathBuf, limit: u64, actual: u64 },
    #[error("generated boot output {path:?} has {actual} bytes, exceeding limit {limit}")]
    GeneratedFileByteLimit { path: PathBuf, limit: usize, actual: usize },
    #[error("generated boot output bytes {actual} exceed limit {limit}")]
    GeneratedTotalByteLimit { limit: usize, actual: usize },
    #[error("logical boot publication bytes {actual} exceed limit {limit}")]
    LogicalByteLimit { limit: u64, actual: u64 },
    #[error("boot publication planning work {actual} exceeds limit {limit}")]
    WorkLimit { limit: usize, actual: usize },
    #[error(
        "boot publication path {path:?} is declared with conflicting content or role under roots {first_root:?} and {second_root:?}"
    )]
    PublicationCollision {
        first_root: ActiveReblitBootDestinationRoot,
        second_root: ActiveReblitBootDestinationRoot,
        path: PathBuf,
    },
    #[error(
        "boot publication paths {first:?} and {second:?} collide case-insensitively under roots {first_root:?} and {second_root:?}"
    )]
    CaseInsensitiveCollision {
        first_root: ActiveReblitBootDestinationRoot,
        second_root: ActiveReblitBootDestinationRoot,
        first: PathBuf,
        second: PathBuf,
    },
    #[error(
        "boot publication file {ancestor:?} under root {ancestor_root:?} is an ancestor of {descendant:?} under root {descendant_root:?}"
    )]
    PublicationHierarchyCollision {
        ancestor_root: ActiveReblitBootDestinationRoot,
        descendant_root: ActiveReblitBootDestinationRoot,
        ancestor: PathBuf,
        descendant: PathBuf,
    },
}
