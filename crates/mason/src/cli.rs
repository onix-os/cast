// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Embeddable command fragments for Cast's package-building commands.
//!
//! Mason deliberately does not own a process entry point.  It never reads
//! process arguments, exits, generates top-level documentation, or handles
//! product-level version and error output.  Cast composes these fragments into
//! its one flat command tree, parses once, and dispatches the selected matches
//! back through this module.

use std::path::PathBuf;

use clap::{ArgMatches, CommandFactory, FromArgMatches};
use thiserror::Error;

use crate::{Env, env};

mod build;
mod cache;
mod chroot;
mod profile;
mod recipe;

/// Explicit Cast-owned options needed to create a Mason command context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    pub verbose: bool,
    pub yes: bool,
    pub cache_dir: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub forge_root: Option<PathBuf>,
}

/// Per-invocation state passed explicitly by Cast after it has parsed the
/// external command line.
pub struct Context {
    env: Env,
    yes: bool,
    verbose: bool,
}

impl Context {
    pub fn new(options: Options) -> Result<Self, Error> {
        let env = Env::new(
            options.cache_dir,
            options.config_dir,
            options.data_dir,
            options.forge_root,
        )?;
        Ok(Self {
            env,
            yes: options.yes,
            verbose: options.verbose,
        })
    }

    /// Construct a context around an already prepared Mason environment.
    ///
    /// This is useful for callers which need to share or inspect the resolved
    /// paths before handing ownership to one command dispatch.
    pub const fn from_env(env: Env, yes: bool, verbose: bool) -> Self {
        Self { env, yes, verbose }
    }
}

/// Return Mason's non-colliding top-level commands for Cast's flat CLI.
///
/// `cache` is returned separately by [`cache_fragment`] because Cast merges
/// Mason's `clean` and `size` operations with Forge's cache operations.
pub fn command_fragments() -> Vec<clap::Command> {
    vec![
        build::Command::command().name("build"),
        chroot::Command::command().name("chroot"),
        profile::Command::command().name("profile"),
        recipe::Command::command().name("recipe"),
    ]
}

/// Return the `cache` fragment containing Mason's `clean` and `size`
/// operations.
///
/// Cast may add Forge-owned operations to this command before attaching it to
/// the external root command.
pub fn cache_fragment() -> clap::Command {
    cache::Command::command().name("cache")
}

/// Dispatch one non-colliding Mason command selected by Cast.
pub fn dispatch(name: &str, matches: &ArgMatches, context: Context) -> Result<(), Error> {
    match name {
        "build" => build::handle(build::Command::from_arg_matches(matches)?, context.env).map_err(Into::into),
        "chroot" => chroot::handle(chroot::Command::from_arg_matches(matches)?, context.env).map_err(Into::into),
        "profile" => profile::handle(profile::Command::from_arg_matches(matches)?, context.env).map_err(Into::into),
        "recipe" => recipe::handle(
            recipe::Command::from_arg_matches(matches)?,
            context.env,
            context.yes,
            context.verbose,
        )
        .map_err(Into::into),
        _ => Err(Error::UnknownCommand { name: name.to_owned() }),
    }
}

/// Dispatch Mason's selected `cache` operation from Cast's merged cache
/// matches.
pub fn dispatch_cache(matches: &ArgMatches, context: Context) -> Result<(), Error> {
    cache::handle(cache::Command::from_arg_matches(matches)?, context.env).map_err(Into::into)
}

#[cfg(feature = "cache-clean-test-support")]
pub(crate) fn run_harness_free_cache_clean_test() {
    cache::run_harness_free_test();
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid Cast command arguments")]
    Arguments(#[from] clap::Error),
    #[error("unknown Cast command {name:?}")]
    UnknownCommand { name: String },
    #[error("build")]
    Build(#[source] Box<build::Error>),
    #[error("cache")]
    Cache(#[from] cache::Error),
    #[error("chroot")]
    Chroot(#[from] chroot::Error),
    #[error("profile")]
    Profile(#[from] profile::Error),
    #[error("environment")]
    Env(#[from] env::Error),
    #[error("recipe")]
    Recipe(#[from] recipe::Error),
}

impl From<build::Error> for Error {
    fn from(error: build::Error) -> Self {
        Self::Build(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_command_fragments_are_unique_and_complete() {
        let fragments = command_fragments();
        assert_eq!(
            fragments.iter().map(clap::Command::get_name).collect::<Vec<_>>(),
            ["build", "chroot", "profile", "recipe"]
        );
        for fragment in fragments {
            fragment.debug_assert();
        }
    }

    #[test]
    fn cache_fragment_owns_only_clean_and_size() {
        let fragment = cache_fragment();
        assert_eq!(fragment.get_name(), "cache");
        assert_eq!(
            fragment
                .get_subcommands()
                .map(clap::Command::get_name)
                .collect::<Vec<_>>(),
            ["clean", "size"]
        );
        fragment.debug_assert();
    }
}
