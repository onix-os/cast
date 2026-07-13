// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{io, path::PathBuf, process};

use crate::{Env, Paths, Recipe, container, recipe};
use clap::Parser;
use thiserror::Error;

#[derive(Debug, Parser)]
#[command(about = "Chroot into the build environment")]
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
    let paths = Paths::new(&recipe, env.cache_dir, "/mason", ".")?;

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
    #[error("io")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_recipe_is_gluon() {
        let command = Command::try_parse_from(["chroot"]).unwrap();

        assert_eq!(command.recipe, PathBuf::from("./stone.glu"));
    }
}
