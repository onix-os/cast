// SPDX-FileCopyrightText: 2026 AerynOS Developers

//! The sole external command surface for OS Tools.

use std::{ffi::OsString, io, path::Path, path::PathBuf};

use clap::{Arg, ArgAction, ArgMatches, Command, value_parser};
use clap_complete::{
    generate_to,
    shells::{Bash, Fish, Zsh},
};
use clap_mangen::Man;
use fs_err as fs;
use thiserror::Error;
use tracing_common::logging::{LogConfig, init_log_with_config};

const MASON_COMMANDS: &[&str] = &["build", "chroot", "profile", "recipe"];

/// Construct Cast's complete, flat command tree.
pub fn command() -> Command {
    let mut cache = mason::cli::cache_fragment();
    cache = cache.subcommand(forge::cli::cache_prune_fragment());

    let mut command = Command::new("cast")
        .about("Declarative package and system delivery")
        .version(tools_buildinfo::get_version())
        .arg_required_else_help(true)
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .global(true)
                .help("Print additional command information")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("yes")
                .short('y')
                .long("yes")
                .global(true)
                .help("Answer yes to confirmation prompts")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("root")
                .short('D')
                .long("directory")
                .global(true)
                .help("Target system root")
                .default_value("/")
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("package_cache_dir")
                .long("package-cache-dir")
                .global(true)
                .help("Package download and asset cache directory")
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("build_cache_dir")
                .long("build-cache-dir")
                .global(true)
                .help("Package build cache directory")
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("config_dir")
                .long("config-dir")
                .global(true)
                .help("Cast configuration directory")
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("data_dir")
                .long("data-dir")
                .global(true)
                .help("Cast policy and data directory")
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("resolver_root")
                .long("resolver-root")
                .global(true)
                .help("Isolated package-resolution root used by builds")
                .value_parser(value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("log")
                .long("log")
                .global(true)
                .help("Logging configuration: <level>[:<format>][:<destination>]")
                .value_parser(value_parser!(LogConfig)),
        )
        .arg(
            Arg::new("generate_manpages")
                .long("generate-manpages")
                .global(true)
                .value_name("DIR")
                .value_parser(value_parser!(PathBuf))
                .hide(true),
        )
        .arg(
            Arg::new("generate_completions")
                .long("generate-completions")
                .global(true)
                .value_name("DIR")
                .value_parser(value_parser!(PathBuf))
                .hide(true),
        )
        .subcommand(cache)
        .subcommand(
            Command::new("version").about("Display Cast version information").arg(
                Arg::new("full")
                    .short('f')
                    .long("full")
                    .help("Print the complete build identity")
                    .action(ArgAction::SetTrue),
            ),
        );

    for fragment in mason::cli::command_fragments() {
        command = command.subcommand(fragment);
    }
    for fragment in forge::cli::command_fragments() {
        command = command.subcommand(fragment);
    }

    command
}

/// Parse and execute a Cast invocation.
pub fn run_from<I, T>(args: I) -> Result<(), Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = command().try_get_matches_from(args)?;

    if let Some(log) = matches.get_one::<LogConfig>("log") {
        init_log_with_config(log.clone());
    }

    if let Some(directory) = matches.get_one::<PathBuf>("generate_manpages") {
        fs::create_dir_all(directory)?;
        generate_manpages(&command(), directory, None)?;
        return Ok(());
    }

    if let Some(directory) = matches.get_one::<PathBuf>("generate_completions") {
        fs::create_dir_all(directory)?;
        generate_completions(&mut command(), directory)?;
        return Ok(());
    }

    let (name, subcommand) = matches.subcommand().ok_or(Error::MissingCommand)?;
    match name {
        "version" => {
            if subcommand.get_flag("full") {
                println!("cast {}", tools_buildinfo::get_full_version());
            } else {
                println!("cast {}", tools_buildinfo::get_simple_version());
            }
            Ok(())
        }
        "cache" => dispatch_cache(&matches, subcommand),
        name if MASON_COMMANDS.contains(&name) => {
            let context = mason::cli::Context::new(mason_options(&matches))?;
            mason::cli::dispatch(name, subcommand, context)?;
            Ok(())
        }
        name => {
            let context = forge_context(&matches);
            if forge::cli::try_dispatch(name, subcommand, &context)? {
                Ok(())
            } else {
                Err(Error::UnknownCommand { name: name.to_owned() })
            }
        }
    }
}

fn dispatch_cache(root: &ArgMatches, cache: &ArgMatches) -> Result<(), Error> {
    let (name, args) = cache.subcommand().ok_or(Error::MissingCacheCommand)?;
    let forge = forge_context(root);
    if forge::cli::try_dispatch_cache(name, args, &forge)? {
        return Ok(());
    }

    let mason = mason::cli::Context::new(mason_options(root))?;
    mason::cli::dispatch_cache(cache, mason)?;
    Ok(())
}

fn mason_options(matches: &ArgMatches) -> mason::cli::Options {
    mason::cli::Options {
        verbose: matches.get_flag("verbose"),
        yes: matches.get_flag("yes"),
        cache_dir: matches.get_one::<PathBuf>("build_cache_dir").cloned(),
        config_dir: matches.get_one::<PathBuf>("config_dir").cloned(),
        data_dir: matches.get_one::<PathBuf>("data_dir").cloned(),
        forge_root: matches.get_one::<PathBuf>("resolver_root").cloned(),
    }
}

fn forge_context(matches: &ArgMatches) -> forge::cli::Context {
    forge::cli::Context::new(
        matches
            .get_one::<PathBuf>("root")
            .expect("Cast supplies a default system root")
            .clone(),
        matches.get_one::<PathBuf>("package_cache_dir").cloned(),
        matches.get_flag("verbose"),
        matches.get_flag("yes"),
    )
}

/// Generate the root and every nested Cast manpage recursively.
pub fn generate_manpages(command: &Command, directory: &Path, prefix: Option<&str>) -> io::Result<()> {
    let name = command.get_name();
    let man = Man::new(command.to_owned());
    let mut buffer = Vec::new();
    man.render(&mut buffer)?;

    let filename = prefix.map_or_else(|| format!("{name}.1"), |prefix| format!("{prefix}-{name}.1"));
    fs::write(directory.join(filename), buffer)?;

    let child_prefix = prefix.map_or_else(|| name.to_owned(), |prefix| format!("{prefix}-{name}"));
    for subcommand in command.get_subcommands() {
        generate_manpages(subcommand, directory, Some(&child_prefix))?;
    }
    Ok(())
}

/// Generate Bash, Fish, and Zsh completions for the sole Cast executable.
pub fn generate_completions(command: &mut Command, directory: &Path) -> io::Result<()> {
    generate_to(Bash, command, "cast", directory)?;
    generate_to(Fish, command, "cast", directory)?;
    generate_to(Zsh, command, "cast", directory)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("command line")]
    Clap(#[from] clap::Error),
    #[error(transparent)]
    Mason(#[from] mason::cli::Error),
    #[error(transparent)]
    Forge(#[from] forge::cli::Error),
    #[error("I/O")]
    Io(#[from] io::Error),
    #[error("a command is required")]
    MissingCommand,
    #[error("a cache command is required")]
    MissingCacheCommand,
    #[error("unknown Cast command {name:?}")]
    UnknownCommand { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_tree_is_flat_unique_and_complete() {
        let command = command();
        command.clone().debug_assert();
        let names = command.get_subcommands().map(Command::get_name).collect::<Vec<_>>();
        for required in MASON_COMMANDS.iter().chain(forge::cli::COMMAND_NAMES) {
            assert!(names.contains(required), "missing Cast command {required}");
        }
        assert!(names.contains(&"cache"));
        assert!(names.contains(&"version"));
        assert!(!names.contains(&"mason"));
        assert!(!names.contains(&"forge"));
    }

    #[test]
    fn shared_cache_contains_each_engine_operation_once() {
        let command = command();
        let cache = command
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == "cache")
            .unwrap();
        assert_eq!(
            cache.get_subcommands().map(Command::get_name).collect::<Vec<_>>(),
            ["clean", "size", "prune"]
        );
    }

    #[test]
    fn generated_documentation_uses_only_the_cast_root_name() {
        let temporary = tempfile::tempdir().unwrap();
        generate_manpages(&command(), temporary.path(), None).unwrap();
        generate_completions(&mut command(), temporary.path()).unwrap();

        assert!(temporary.path().join("cast.1").is_file());
        assert!(temporary.path().join("cast-build.1").is_file());
        assert!(!temporary.path().join("boulder.1").exists());
        assert!(!temporary.path().join("moss.1").exists());
    }
}
