// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    io,
    path::{Path, PathBuf},
};

use clap::{Args, Parser};
use container::Container;
use forge::util;
use humansize::BINARY;
use rayon::iter::{ParallelBridge, ParallelIterator};
use thiserror::Error;
use walkdir::WalkDir;

use crate::Env;

#[derive(Debug, Parser)]
#[command(about = "Manage Cast build caches")]
pub struct Command {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    subcommand: Subcommand,
}

#[derive(Debug, Args)]
pub struct Global {
    #[arg(
        short,
        long = "build-cache",
        help = "Select the package-build cache",
        default_value_t = false,
        global = true
    )]
    pub build_cache: bool,
    #[arg(
        short = 'p',
        long = "package-cache",
        help = "Select the package-resolution cache used by builds",
        default_value_t = false,
        global = true
    )]
    pub package_cache: bool,
}

#[derive(Debug, clap::Subcommand)]
pub enum Subcommand {
    #[command(about = "Clean out the cache(s) for the current environment")]
    Clean,
    #[command(about = "Show the cache size(s) for the current environment")]
    Size,
}

pub fn handle(command: Command, env: Env) -> Result<(), Error> {
    let build_cache = command.global.build_cache;
    let package_cache = command.global.package_cache;

    match command.subcommand {
        Subcommand::Clean => clean(env, build_cache, package_cache),
        Subcommand::Size => size(env, build_cache, package_cache),
    }
}

fn clean(env: Env, build_cache: bool, package_cache: bool) -> Result<(), Error> {
    for (name, path) in selected_caches(&env, build_cache, package_cache) {
        let tmpdir = tempfile::tempdir()?;

        println!("Deleting {name} directory: {}", path.display());

        Container::new(tmpdir.path())
            .bind_rw(&path, Path::new("/remove"))
            .run(|| util::par_remove_dir_all(Path::new("/remove")))?;
    }
    Ok(())
}

fn size(env: Env, build_cache: bool, package_cache: bool) -> Result<(), Error> {
    for (name, path) in selected_caches(&env, build_cache, package_cache) {
        let size: u64 = WalkDir::new(&path)
            .into_iter()
            .par_bridge()
            .filter_map(Result::ok)
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();
        println!("{name} ({}): {}", path.display(), humansize::format_size(size, BINARY));
    }
    Ok(())
}

fn selected_caches(env: &Env, build_cache: bool, package_cache: bool) -> Vec<(&'static str, PathBuf)> {
    let select_all = !build_cache && !package_cache;
    let mut v = Vec::new();
    if select_all || build_cache {
        v.push(("build cache", env.cache_dir.to_owned()));
    }
    if select_all || package_cache {
        v.push(("package cache", env.forge_dir.to_owned()));
    }
    v
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("container")]
    Container(#[from] container::Error),
    #[error("io")]
    Io(#[from] io::Error),
}
