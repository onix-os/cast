// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

pub use self::architecture::Architecture;
pub use self::env::Env;
pub use self::paths::Paths;
pub use self::policy::BuildPolicy;
pub use self::profile::Profile;
pub use self::recipe::Recipe;
pub use self::timing::Timing;

mod architecture;
mod build;
mod build_lock;
pub mod cli;
mod container;
mod draft;
mod env;
mod executor;
mod generated_lock;
mod package;
mod paths;
mod planner;
mod policy;
mod profile;
mod recipe;
pub mod source_lock;
mod timing;
mod upstream;

#[cfg(test)]
pub(crate) fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("create private test directory");
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
        .expect("normalize private test directory");
    directory
}
