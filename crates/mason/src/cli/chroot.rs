// SPDX-FileCopyrightText: 2024 AerynOS Developers

//! Impure interactive development access to an existing build root.
//!
//! This command deliberately stays outside frozen planning and execution. It
//! never invokes, validates, or syncs package emission; files written by the
//! shell are not frozen build artifacts.

use std::{io, path::PathBuf, process};

use crate::{BuildPolicy, Env, Paths, Recipe, container, policy, recipe};
use clap::Parser;
use stone_recipe::derivation::BuilderLayout;
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(
    about = "Open an impure interactive development shell",
    long_about = "Open an impure interactive development shell in an existing build root. This command is outside frozen-build reproducibility guarantees. It never invokes, validates, or syncs package emission; files created by the shell are not frozen build artifacts."
)]
pub struct Command {
    #[arg(
        default_value = "./stone.glu",
        help = "Path to a stone.glu recipe file or recipe directory"
    )]
    recipe: PathBuf,
}

pub fn handle(command: Command, env: Env) -> Result<(), Error> {
    let Command { recipe: recipe_path } = command;

    let recipe = Recipe::load(recipe_path)?;
    let policy = BuildPolicy::load(&env)?;
    let layout = BuilderLayout::from_policy(&policy.spec.sandbox, &policy.spec.build_root.compiler_cache);
    let paths = Paths::new(&recipe, layout, env.cache_dir, ".")?;

    let rootfs = paths.rootfs().host;

    // Has rootfs been setup?
    if !rootfs.join("usr").exists() {
        return Err(Error::MissingRootFs);
    }

    let home = &paths.build().guest;

    container::exec(&paths, recipe.declaration.options.networking, || {
        let mut child = process::Command::new("/bin/bash")
            .arg("--login")
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/usr/sbin")
            .env("TERM", "xterm-256color")
            .spawn()?;

        child.wait()?;

        Ok(()) as io::Result<_>
    })?;

    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("build root doesn't exist, make sure to run build first")]
    MissingRootFs,
    #[error("container")]
    Container(#[from] container::Error),
    #[error("recipe")]
    Recipe(#[from] recipe::Error),
    #[error("build policy")]
    Policy(#[from] policy::Error),
    #[error("io")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::*;

    #[test]
    fn default_recipe_is_gluon() {
        let command = Command::try_parse_from(["chroot"]).unwrap();

        assert_eq!(command.recipe, PathBuf::from("./stone.glu"));
    }

    #[test]
    fn help_marks_chroot_as_an_impure_non_frozen_development_command() {
        let command = Command::command();

        assert_eq!(
            command.get_about().map(ToString::to_string).as_deref(),
            Some("Open an impure interactive development shell")
        );
        let long_about = command.get_long_about().unwrap().to_string();
        assert!(long_about.contains("outside frozen-build reproducibility guarantees"));
        assert!(long_about.contains("never invokes, validates, or syncs package emission"));
        assert!(long_about.contains("not frozen build artifacts"));
    }
}
