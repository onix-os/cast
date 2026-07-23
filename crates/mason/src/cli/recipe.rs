// SPDX-FileCopyrightText: 2024 AerynOS Developers
use std::{
    io::{self, Write},
    num::{NonZeroU32, NonZeroU64},
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    Env,
    draft::{self, Drafter},
    planner, profile, recipe,
    source_lock::{SOURCE_LOCK_FILE_NAME, WriteOutcome},
    upstream::ARCHIVE_DOWNLOAD_LIMITS,
};
use clap::{Args, Parser};
use forge::{request, runtime};
use fs_err::{self as fs};
use stone_recipe::{UpstreamSpec, spec::UpstreamValidationError, upstream};
use tempfile::NamedTempFile;
use thiserror::Error;
use tui::{MultiProgress, ProgressBar, ProgressStyle, Styled};
use url::Url;
use version_parse::VersionExtractor;

mod explanation;

const LONG_UPDATE_ABOUT: &str = concat!(
    "Refresh generated source resolution or suggest authored changes\n\n",
    "Cast typechecks the recipe and reports explicit field changes, but never\n",
    "rewrites authored Gluon. Apply the suggestions to stone.glu manually.\n\n",
    "With no --ver or --upstream values, Cast resolves the current authored\n",
    "upstreams and atomically refreshes sources.lock.glu. Provide --ver and/or one\n",
    "or more --upstream values to request authored changes. When only a plain archive\n",
    "upstream is supplied, Cast derives the version from its URL. After applying\n",
    "upstream edits, run this command again without update values to regenerate the\n",
    "lock and bind the new resolution into provenance."
);

#[derive(Debug, Parser)]
#[command(about = "Utilities to create and manipulate stone recipe files")]
pub struct Command {
    #[command(subcommand)]
    subcommand: Subcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum Subcommand {
    #[command(about = "Typecheck and validate a recipe without building it")]
    Check {
        #[arg(
            default_value = "./stone.glu",
            help = "Path to a stone.glu recipe file or recipe directory"
        )]
        recipe: PathBuf,
    },
    #[command(about = "Freeze and print a canonical target-specific derivation plan")]
    Plan(PlanCommand),
    #[command(about = "Explain derivation identity and input provenance")]
    Explain(ExplainCommand),
    #[command(about = "Evaluate and print the concrete normalized package-v3 declaration")]
    Eval {
        #[arg(
            default_value = "./stone.glu",
            help = "Path to a package-v3 stone.glu file or recipe directory"
        )]
        recipe: PathBuf,
    },
    #[command(about = "Suggest a release bump without rewriting authored Gluon")]
    Bump {
        #[arg(
            short,
            long,
            default_value = "./stone.glu",
            help = "Authored Gluon recipe to validate and inspect"
        )]
        recipe: PathBuf,
        #[arg(
            short = 'n',
            long,
            required = false,
            help = "Set release to a specific number instead of incrementing by one"
        )]
        release: Option<u64>,
    },
    #[command(about = "Create a skeletal stone.glu recipe from source archive URIs")]
    New {
        #[arg(short, long, default_value = ".", help = "Location to output generated files")]
        output: PathBuf,
        #[arg(required = true, value_name = "URI", help = "Source archive URIs")]
        upstreams: Vec<Url>,
    },
    #[command(about = LONG_UPDATE_ABOUT)]
    Update {
        #[arg(id = "recipe_version", long = "ver", required = false, help = "Update version")]
        version: Option<String>,
        #[arg(
            short = 'u',
            long = "upstream",
            required = false,
            value_parser = parse_updated_source,
            help = concat!(
                "Update upstream source, can be passed multiple times.\n",
                "Applied in same order as defined in recipe file.\n",
                "To update a Git upstream,\n",
                "Use the \"git|commit_or_tag\" syntax.\n\n",
                "Example:\n",
                " -u \"https://some.plan/file.tar.gz\" -u \"git|v1.1\"")
        )]
        upstreams: Vec<UpdatedSource>,
        #[arg(
            default_value = "./stone.glu",
            help = "Authored Gluon recipe to validate and inspect"
        )]
        recipe: PathBuf,
        #[arg(
            long,
            default_value = "false",
            help = "Don't suggest incrementing the release number"
        )]
        no_bump: bool,
    },
}

#[derive(Debug, Args)]
pub struct PlanCommand {
    #[arg(default_value = "./stone.glu", help = "Authored Gluon package factory")]
    recipe: PathBuf,
    #[arg(long, default_value = "default-x86_64", help = "Explicit Cast repository profile")]
    profile: profile::Id,
    #[arg(long, help = "Exact build target, for example x86_64 or emul32/x86_64")]
    target: String,
    #[arg(long, help = "Explicit reproducible source timestamp")]
    source_date_epoch: i64,
    #[arg(long, default_value = "1", help = "Build release recorded in the derivation")]
    build_release: NonZeroU64,
    #[arg(
        long,
        default_value = "1",
        help = "Explicit parallel job count exposed to build steps"
    )]
    jobs: NonZeroU32,
    #[arg(long = "compiler-cache", default_value_t = false)]
    compiler_cache: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Resolve and atomically refresh build.lock.glu"
    )]
    update_lock: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Refresh repositories before updating build.lock.glu"
    )]
    refresh_repositories: bool,
}

#[derive(Debug, Args)]
pub struct ExplainCommand {
    #[arg(default_value = "./stone.glu", help = "Authored Gluon package factory")]
    recipe: PathBuf,
    #[arg(long, default_value = "default-x86_64", help = "Explicit Cast repository profile")]
    profile: profile::Id,
    #[arg(long, help = "Exact build target, for example x86_64 or emul32/x86_64")]
    target: String,
    #[arg(long, help = "Explicit reproducible source timestamp")]
    source_date_epoch: i64,
    #[arg(long, default_value = "1", help = "Build release recorded in the derivation")]
    build_release: NonZeroU64,
    #[arg(
        long,
        default_value = "1",
        help = "Explicit parallel job count exposed to build steps"
    )]
    jobs: NonZeroU32,
    #[arg(long = "compiler-cache", default_value_t = false)]
    compiler_cache: bool,
}

/// A new source for an existing recipe.
#[derive(Clone, Debug)]
pub enum UpdatedSource {
    /// The new source is a regular URL that points
    /// to a source archive.
    Plain(Url),
    /// The new source is a Git reference (i.e. commit hash
    /// or tag) in the Git repository referenced in the recipe.
    Git(String),
}

fn parse_updated_source(s: &str) -> Result<UpdatedSource, String> {
    match s.strip_prefix(upstream::GIT_PREFIX) {
        Some(git_ref) => Ok(UpdatedSource::Git(git_ref.to_owned())),
        None => Ok(UpdatedSource::Plain(s.parse::<Url>().map_err(|e| e.to_string())?)),
    }
}

pub fn handle(command: Command, env: Env, _yes: bool, _verbose: bool) -> Result<(), Error> {
    match command.subcommand {
        Subcommand::Check { recipe } => check(recipe),
        Subcommand::Plan(command) => plan(env, command),
        Subcommand::Explain(command) => explain(env, command),
        Subcommand::Eval { recipe } => eval(recipe),
        Subcommand::Bump { recipe, release } => bump(recipe, release),
        Subcommand::New { output, upstreams } => new(env, output, upstreams),
        Subcommand::Update {
            recipe,
            version,
            upstreams,
            no_bump,
        } => update(env, &recipe, version, upstreams, no_bump),
    }
}

fn plan(env: Env, command: PlanCommand) -> Result<(), Error> {
    let planned = planner::plan(
        env,
        planner::Request {
            recipe: command.recipe,
            profile: command.profile,
            target: command.target,
            source_date_epoch: command.source_date_epoch,
            build_release: command.build_release,
            jobs: command.jobs,
            compiler_cache: command.compiler_cache,
            update_lock: command.update_lock,
            refresh_repositories: command.refresh_repositories,
        },
    )?;
    if let Some(outcome) = planned.lock_outcome {
        println!("build_lock = {outcome:?} ({})", planned.lock_path.display());
    }
    println!("derivation_id = {:?}", planned.plan.derivation_id().as_str());
    println!(
        "request_fingerprint = {:?}",
        planned.plan.build_lock.request_fingerprint
    );
    println!("target = {:?}", planned.plan.build_lock.target.name);
    println!("source_date_epoch = {}", planned.plan.source_date_epoch);
    println!("packages = {}", planned.plan.build_lock.packages.len());
    println!("jobs = {}", planned.plan.jobs.len());
    println!(
        "phases = {}",
        planned.plan.jobs.iter().map(|job| job.phases.len()).sum::<usize>()
    );
    println!("outputs = {}", planned.plan.outputs.len());
    println!("canonical_plan = {:?}", hex::encode(planned.plan.canonical_bytes()));
    Ok(())
}

fn explain(env: Env, command: ExplainCommand) -> Result<(), Error> {
    let planned = planner::plan(
        env,
        planner::Request {
            recipe: command.recipe,
            profile: command.profile,
            target: command.target,
            source_date_epoch: command.source_date_epoch,
            build_release: command.build_release,
            jobs: command.jobs,
            compiler_cache: command.compiler_cache,
            update_lock: false,
            refresh_repositories: false,
        },
    )?;
    print!("{}", explanation::format(&planned.plan));
    Ok(())
}

fn check(path: PathBuf) -> Result<(), Error> {
    let recipe = recipe::Recipe::load(path).map_err(Error::CheckRecipe)?;
    println!(
        "{} | {} is valid ({})",
        "Recipe".green(),
        recipe.path.display(),
        recipe.fingerprint.sha256
    );
    Ok(())
}

fn eval(path: PathBuf) -> Result<(), Error> {
    let recipe = recipe::Recipe::load(path).map_err(Error::CheckRecipe)?;
    println!("package-v3-evaluation {{");
    println!("  recipe = {:?}", recipe.path.display().to_string());
    println!("  fingerprint = {:?}", recipe.fingerprint.sha256);
    println!("  declaration = {:#?}", recipe.declaration);
    println!("}}");
    Ok(())
}

fn bump(recipe: PathBuf, release: Option<u64>) -> Result<(), Error> {
    let recipe = load_authored_gluon(&recipe)?;
    let previous = u64::try_from(recipe.declaration.meta.release).expect("validated package release");
    let proposed = match release {
        Some(release) => release,
        None => previous.checked_add(1).ok_or(Error::ReleaseOverflow)?,
    };
    if proposed == 0 {
        return Err(Error::InvalidRelease(proposed));
    }
    if proposed == previous {
        println!("{} already has release {previous}", recipe.path.display());
        return Ok(());
    }

    require_manual_edit(
        &recipe.path,
        vec![SuggestedChange::new("meta.release", previous, proposed)],
    )
}

fn new(env: Env, output: PathBuf, upstreams: Vec<Url>) -> Result<(), Error> {
    const RECIPE_FILE: &str = "stone.glu";

    generate_new_recipe(&output, RECIPE_FILE, || Drafter::new(env, upstreams).run())?;
    println!("Saved {RECIPE_FILE} to {output:?}");
    Ok(())
}

fn generate_new_recipe(
    output: &Path,
    recipe_file: &str,
    draft: impl FnOnce() -> Result<draft::Draft, draft::Error>,
) -> Result<(), Error> {
    prepare_new_output(output, recipe_file)?;
    let draft = draft()?;
    ensure_new_output(output, recipe_file)?;
    publish_new_recipe(output, recipe_file, draft.stone.as_bytes())
}

fn prepare_new_output(output: &Path, recipe_file: &str) -> Result<(), Error> {
    match fs::symlink_metadata(output) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(Error::OutputNotDirectory(output.to_owned())),
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(Error::CreateDir(source)),
    }
    let target = output.join(recipe_file);
    match fs::symlink_metadata(&target) {
        Ok(_) => Err(Error::RecipeAlreadyExists(target)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Write(source)),
    }
}

fn ensure_new_output(output: &Path, recipe_file: &str) -> Result<(), Error> {
    if matches!(fs::symlink_metadata(output), Err(source) if source.kind() == io::ErrorKind::NotFound) {
        fs::create_dir_all(output).map_err(Error::CreateDir)?;
    }
    prepare_new_output(output, recipe_file)
}

fn publish_new_recipe(output: &Path, recipe_file: &str, bytes: &[u8]) -> Result<(), Error> {
    let target = output.join(recipe_file);
    let mut staging = tempfile::Builder::new()
        .prefix(".cast-recipe-")
        .tempfile_in(output)
        .map_err(Error::Write)?;
    staging.write_all(bytes).map_err(Error::Write)?;
    staging
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o644))
        .map_err(Error::Write)?;
    staging.as_file().sync_all().map_err(Error::Write)?;
    staging
        .persist_noclobber(&target)
        .map_err(|error| Error::InstallRecipe {
            target,
            source: error.error,
        })?;
    fs::File::open(output)
        .and_then(|directory| directory.sync_all())
        .map_err(Error::Write)
}

fn update(
    env: Env,
    recipe_path: &Path,
    version: Option<String>,
    sources: Vec<UpdatedSource>,
    no_bump: bool,
) -> Result<(), Error> {
    if version.is_none() && sources.is_empty() {
        return refresh_source_lock(&env, recipe_path);
    }
    let recipe = load_authored_gluon(recipe_path)?;

    if sources.len() > recipe.declaration.sources.len() {
        return Err(Error::TooManyUpstreamUpdates {
            supplied: sources.len(),
            available: recipe.declaration.sources.len(),
        });
    }

    let proposed_version = match (version, sources.first()) {
        (Some(version), _) => version,
        (None, Some(UpdatedSource::Plain(new_uri))) => {
            let parsed = VersionExtractor::new().extract(new_uri.as_str())?;
            println!(
                "No version provided; derived {} from the first upstream URL",
                parsed.version
            );
            parsed.version
        }
        (None, Some(UpdatedSource::Git(_))) => return Err(Error::GitUpstreamMustProvideVersion),
        (None, None) => unreachable!("an explicit version or upstream was checked above"),
    };

    let mut changes = Vec::new();
    if proposed_version != recipe.declaration.meta.version {
        changes.push(SuggestedChange::new(
            "meta.version",
            &recipe.declaration.meta.version,
            proposed_version,
        ));
    }
    if !no_bump {
        let release = u64::try_from(recipe.declaration.meta.release).expect("validated package release");
        let proposed_release = release.checked_add(1).ok_or(Error::ReleaseOverflow)?;
        changes.push(SuggestedChange::new("meta.release", release, proposed_release));
    }

    preflight_updated_sources(&recipe.declaration.sources, &sources)?;
    let mpb = MultiProgress::new();
    for (index, (original, update)) in recipe.declaration.sources.iter().zip(sources).enumerate() {
        match (original, update) {
            (UpstreamSpec::Archive { .. }, UpdatedSource::Git(_)) => {
                return Err(Error::UpstreamMismatch(index, "Plain", "Git"));
            }
            (UpstreamSpec::Git { .. }, UpdatedSource::Plain(_)) => {
                return Err(Error::UpstreamMismatch(index, "Git", "Plain"));
            }
            (UpstreamSpec::Archive { url, hash, .. }, UpdatedSource::Plain(new_uri)) => {
                let new_hash = runtime::block_on(fetch_and_cache_upstream(&env, new_uri.clone(), index, &mpb))?;
                if url != new_uri.as_str() {
                    changes.push(SuggestedChange::new(
                        format!("sources[{index}].url"),
                        url,
                        new_uri.as_str(),
                    ));
                }
                if *hash != new_hash {
                    changes.push(SuggestedChange::new(format!("sources[{index}].hash"), hash, new_hash));
                }
            }
            (UpstreamSpec::Git { git_ref, .. }, UpdatedSource::Git(new_ref)) => {
                if *git_ref != new_ref {
                    changes.push(SuggestedChange::new(
                        format!("sources[{index}].git_ref"),
                        git_ref,
                        new_ref,
                    ));
                }
            }
        }
    }
    let _ = mpb.clear();

    if changes.is_empty() {
        println!(
            "{} already matches the requested authored values",
            recipe.path.display()
        );
        return Ok(());
    }

    require_manual_edit(&recipe.path, changes)
}

fn preflight_updated_sources(originals: &[UpstreamSpec], updates: &[UpdatedSource]) -> Result<(), Error> {
    for (index, (original, update)) in originals.iter().zip(updates).enumerate() {
        match (original, update) {
            (UpstreamSpec::Archive { .. }, UpdatedSource::Git(_)) => {
                return Err(Error::UpstreamMismatch(index, "Plain", "Git"));
            }
            (UpstreamSpec::Git { .. }, UpdatedSource::Plain(_)) => {
                return Err(Error::UpstreamMismatch(index, "Git", "Plain"));
            }
            (UpstreamSpec::Archive { .. }, UpdatedSource::Plain(uri)) => {
                UpstreamSpec::Archive {
                    url: uri.to_string(),
                    hash: "0".repeat(64),
                    rename: None,
                    strip_dirs: None,
                    unpack: true,
                    unpack_dir: None,
                }
                .validate()
                .map_err(|source| Error::InvalidUpdatedUpstream { index, source })?;
            }
            (UpstreamSpec::Git { .. }, UpdatedSource::Git(_)) => {}
        }
    }
    Ok(())
}

fn refresh_source_lock(env: &Env, recipe_path: &Path) -> Result<(), Error> {
    let recipe = recipe::Recipe::load_authored(recipe_path).map_err(Error::LoadRecipe)?;
    let storage_dir = env.cache_dir.join("upstreams");
    fs::create_dir_all(&storage_dir).map_err(Error::CreateDir)?;
    let outcome = crate::upstream::refresh_source_lock(&recipe, &storage_dir).map_err(Error::RefreshSourceLock)?;
    let lock_path = recipe.path.with_file_name(SOURCE_LOCK_FILE_NAME);

    match outcome {
        WriteOutcome::Written => println!("Refreshed {}", lock_path.display()),
        WriteOutcome::Unchanged => println!("{} is already current", lock_path.display()),
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuggestedChange {
    field: String,
    current: String,
    proposed: String,
}

impl SuggestedChange {
    fn new(field: impl Into<String>, current: impl ToString, proposed: impl ToString) -> Self {
        Self {
            field: field.into(),
            current: current.to_string(),
            proposed: proposed.to_string(),
        }
    }
}

fn load_authored_gluon(path: &Path) -> Result<recipe::Recipe, Error> {
    let recipe = match recipe::Recipe::load(path) {
        Ok(recipe) => recipe,
        Err(recipe::Error::StaleSourceLock { path, source }) => {
            return Err(Error::StaleGeneratedLock { path, source });
        }
        Err(error) => return Err(Error::LoadRecipe(error)),
    };
    Ok(recipe)
}

fn require_manual_edit(path: &Path, changes: Vec<SuggestedChange>) -> Result<(), Error> {
    let lock_refresh_required = changes.iter().any(|change| change.field.starts_with("sources["));
    println!("suggested_authored_changes {{");
    println!("  recipe = {:?}", path.display().to_string());
    for change in changes {
        println!("  change {{");
        println!("    field = {:?}", change.field);
        println!("    current = {:?}", change.current);
        println!("    proposed = {:?}", change.proposed);
        println!("  }}");
    }
    if lock_refresh_required {
        println!("  generated_lock_remediation {{");
        println!("    artifact = {SOURCE_LOCK_FILE_NAME:?}");
        println!("    action = \"after applying the upstream edits, run `cast recipe update` without update values\"");
        println!("  }}");
    }
    println!("}}");
    Err(Error::ManualEditRequired(path.to_owned()))
}

/// Fetches the upstream at `uri` and caches it so it doesn't need to be refetched
/// when this recipe is finally built.
///
/// Returns the sha256 hash of the fetched upstream
async fn fetch_and_cache_upstream(env: &Env, uri: Url, index: usize, mpb: &MultiProgress) -> Result<String, Error> {
    let pb = mpb.add(
        ProgressBar::new(u64::MAX)
            .with_message(format!("{} {}", "Fetching".blue(), uri.as_str().bold()))
            .with_style(
                ProgressStyle::with_template(" {spinner} {wide_msg} {binary_bytes_per_sec:>.dim} ")
                    .unwrap()
                    .tick_chars("--=≡■≡=--"),
            ),
    );
    pb.enable_steady_tick(Duration::from_millis(150));

    let temp_file_path = NamedTempFile::with_prefix("cast-")
        .map_err(Error::CreateTempFile)?
        .into_temp_path();

    let hash = request::download_with_progress_and_sha256_and_limits(
        uri.clone(),
        &temp_file_path,
        ARCHIVE_DOWNLOAD_LIMITS,
        |progress| {
            pb.inc(progress.delta);
        },
    )
    .await?;

    crate::upstream::admit_downloaded_archive(&env.cache_dir.join("upstreams"), uri, &hash, &temp_file_path, index)
        .map_err(Error::CacheUpstream)?;
    drop(temp_file_path);

    pb.finish();
    mpb.remove(&pb);

    Ok(hash)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("check recipe")]
    CheckRecipe(#[source] recipe::Error),
    #[error("plan derivation")]
    Planner(#[source] Box<planner::Error>),
    #[error("load and validate authored recipe")]
    LoadRecipe(#[source] recipe::Error),
    #[error(
        "generated source lock {path:?} is stale; run `cast recipe update` without update values to atomically refresh it"
    )]
    StaleGeneratedLock {
        path: PathBuf,
        #[source]
        source: Box<crate::source_lock::ValidationError>,
    },
    #[error(
        "manual authored edit required for {0}; Cast validated the recipe and intentionally left it byte-for-byte unchanged"
    )]
    ManualEditRequired(PathBuf),
    #[error("refresh generated source lock")]
    RefreshSourceLock(#[source] crate::upstream::Error),
    #[error("release cannot be incremented beyond the u64 range")]
    ReleaseOverflow,
    #[error("release must be greater than zero (found {0})")]
    InvalidRelease(u64),
    #[error("received {supplied} upstream updates but the recipe declares only {available} upstreams")]
    TooManyUpstreamUpdates { supplied: usize, available: usize },
    #[error("Mismatch for upstream[{0}], expected {1} got {2}")]
    UpstreamMismatch(usize, &'static str, &'static str),
    #[error("writing recipe")]
    Write(#[source] io::Error),
    #[error("recipe output is not a directory: {0}")]
    OutputNotDirectory(PathBuf),
    #[error("refusing to replace existing recipe: {0}")]
    RecipeAlreadyExists(PathBuf),
    #[error("atomically install generated recipe at {target}")]
    InstallRecipe {
        target: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("creating output directory")]
    CreateDir(#[source] io::Error),
    #[error("create temp file")]
    CreateTempFile(#[source] io::Error),
    #[error("admit fetched upstream to cache")]
    CacheUpstream(#[source] crate::upstream::Error),
    #[error("updated source archive {index} is invalid: {source}")]
    InvalidUpdatedUpstream {
        index: usize,
        #[source]
        source: UpstreamValidationError,
    },
    #[error("fetch upstream")]
    Fetch(#[from] request::Error),
    #[error("invalid utf-8 input")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("draft")]
    Draft(#[from] draft::Error),
    #[error("version parse")]
    Upstreams(#[from] version_parse::VersionError),
    #[error("Must provide version if first upstream provided is of type git")]
    GitUpstreamMustProvideVersion,
    #[error("io")]
    Io(#[from] io::Error),
}

impl From<planner::Error> for Error {
    fn from(error: planner::Error) -> Self {
        Self::Planner(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    const AUTHORED_EXPRESSION: &str = r#"let cast = import! cast.package.v3
let release = 1
let version = "1.2.3"
cast.mk_package (cast.meta {
    pname = "example",
    version,
    release,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
})
"#;

    const AUTHORED_WITH_ARCHIVE: &str = r#"let cast = import! cast.package.v3
let base = cast.mk_package (cast.meta {
    pname = "example",
    version = "1.2.3",
    release = 1,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
})
{
    sources = [cast.source.archive
        "https://example.com/source.tar.xz"
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"],
    .. base
}
"#;

    fn environment(root: &Path) -> Env {
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
        Env::new(
            Some(root.join("cache")),
            Some(root.to_owned()),
            Some(root.join("data")),
            Some(root.join("forge")),
        )
        .unwrap()
    }

    #[test]
    fn all_recipe_inputs_default_to_gluon() {
        let check = Command::try_parse_from(["recipe", "check"]).unwrap();
        assert!(matches!(
            check.subcommand,
            Subcommand::Check { recipe } if recipe == Path::new("./stone.glu")
        ));

        let eval = Command::try_parse_from(["recipe", "eval"]).unwrap();
        assert!(matches!(
            eval.subcommand,
            Subcommand::Eval { recipe } if recipe == Path::new("./stone.glu")
        ));

        let bump = Command::try_parse_from(["recipe", "bump"]).unwrap();
        assert!(matches!(
            bump.subcommand,
            Subcommand::Bump { recipe, .. } if recipe == Path::new("./stone.glu")
        ));

        let update = Command::try_parse_from(["recipe", "update"]).unwrap();
        assert!(matches!(
            update.subcommand,
            Subcommand::Update { recipe, .. } if recipe == Path::new("./stone.glu")
        ));

        let plan = Command::try_parse_from([
            "recipe",
            "plan",
            "--target",
            "x86_64",
            "--source-date-epoch",
            "1700000000",
        ])
        .unwrap();
        assert!(matches!(
            plan.subcommand,
            Subcommand::Plan(PlanCommand { recipe, jobs, .. })
                if recipe == Path::new("./stone.glu") && jobs.get() == 1
        ));
    }

    #[test]
    fn bump_suggests_a_manual_change_without_mutating_authored_expression() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stone.glu");
        fs::write(&path, AUTHORED_EXPRESSION).unwrap();

        let error = bump(path.clone(), None).unwrap_err();

        assert!(
            matches!(error, Error::ManualEditRequired(ref error_path) if error_path == &path.canonicalize().unwrap())
        );
        assert_eq!(fs::read_to_string(path).unwrap(), AUTHORED_EXPRESSION);
    }

    #[test]
    fn update_suggests_manual_changes_without_mutating_authored_expression() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stone.glu");
        fs::write(&path, AUTHORED_EXPRESSION).unwrap();

        let error = update(
            environment(root.path()),
            &path,
            Some("2.0.0".to_owned()),
            Vec::new(),
            false,
        )
        .unwrap_err();

        assert!(
            matches!(error, Error::ManualEditRequired(ref error_path) if error_path == &path.canonicalize().unwrap())
        );
        assert_eq!(fs::read_to_string(path).unwrap(), AUTHORED_EXPRESSION);
    }

    #[test]
    fn failed_draft_leaves_an_absent_output_directory_absent() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("new-package");

        let error = generate_new_recipe(&output, "stone.glu", || {
            Err(draft::Error::Io(io::Error::other("draft failed")))
        })
        .unwrap_err();

        assert!(matches!(error, Error::Draft(_)));
        assert!(!output.exists());
    }

    #[test]
    fn unsupported_detected_builder_publishes_no_recipe_or_output_directory() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("new-package");

        let error = generate_new_recipe(&output, "stone.glu", || {
            Err(draft::Error::UnsupportedDraftSystem {
                system: "python-pep517".to_owned(),
            })
        })
        .unwrap_err();

        assert!(matches!(
            error,
            Error::Draft(draft::Error::UnsupportedDraftSystem { .. })
        ));
        assert!(!output.exists());
    }

    #[test]
    fn undetected_builder_publishes_no_recipe_or_output_directory() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("new-package");

        let error = generate_new_recipe(&output, "stone.glu", || Err(draft::Error::UndetectedBuildSystem)).unwrap_err();

        assert!(matches!(error, Error::Draft(draft::Error::UndetectedBuildSystem)));
        assert!(!output.exists());
    }

    #[test]
    fn existing_recipe_is_untouched_and_drafting_never_starts() {
        use std::cell::Cell;

        let root = tempfile::tempdir().unwrap();
        let recipe_path = root.path().join("stone.glu");
        fs::write(&recipe_path, b"authored bytes").unwrap();
        let drafted = Cell::new(false);

        let error = generate_new_recipe(root.path(), "stone.glu", || {
            drafted.set(true);
            Ok(draft::Draft {
                stone: "replacement".to_owned(),
            })
        })
        .unwrap_err();

        assert!(matches!(error, Error::RecipeAlreadyExists(path) if path == recipe_path));
        assert!(!drafted.get());
        assert_eq!(fs::read(recipe_path).unwrap(), b"authored bytes");
    }

    #[test]
    fn recipe_created_during_drafting_wins_and_is_never_replaced() {
        let root = tempfile::tempdir().unwrap();
        let recipe_path = root.path().join("stone.glu");

        let error = generate_new_recipe(root.path(), "stone.glu", || {
            fs::write(&recipe_path, b"raced bytes").unwrap();
            Ok(draft::Draft {
                stone: "generated replacement".to_owned(),
            })
        })
        .unwrap_err();

        assert!(matches!(error, Error::RecipeAlreadyExists(path) if path == recipe_path));
        assert_eq!(fs::read(recipe_path).unwrap(), b"raced bytes");
    }

    #[test]
    fn successful_generated_recipe_is_published_with_exact_mode() {
        use std::os::unix::fs::MetadataExt as _;

        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("new-package");

        generate_new_recipe(&output, "stone.glu", || {
            Ok(draft::Draft {
                stone: "generated bytes".to_owned(),
            })
        })
        .unwrap();

        let recipe = output.join("stone.glu");
        assert_eq!(fs::read(&recipe).unwrap(), b"generated bytes");
        assert_eq!(fs::metadata(recipe).unwrap().mode() & 0o7777, 0o644);
    }

    #[test]
    fn update_without_authored_changes_atomically_refreshes_the_generated_lock() {
        let root = tempfile::tempdir().unwrap();
        let recipe_path = root.path().join("stone.glu");
        let lock_path = root.path().join(SOURCE_LOCK_FILE_NAME);
        fs::write(&recipe_path, AUTHORED_EXPRESSION).unwrap();
        fs::write(&lock_path, "stale generated bytes").unwrap();

        update(environment(root.path()), &recipe_path, None, Vec::new(), false).unwrap();

        let first_metadata = fs::metadata(&lock_path).unwrap();
        let lock =
            crate::source_lock::decode_source_lock(SOURCE_LOCK_FILE_NAME, &fs::read(&lock_path).unwrap()).unwrap();
        assert!(lock.sources.is_empty());
        assert_eq!(fs::read_to_string(&recipe_path).unwrap(), AUTHORED_EXPRESSION);

        update(environment(root.path()), &recipe_path, None, Vec::new(), false).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(first_metadata.ino(), fs::metadata(lock_path).unwrap().ino());
        }
    }

    #[test]
    fn stale_generated_lock_reports_refresh_remediation_without_mutating_source() {
        use crate::source_lock::{ArchiveResolution, SourceLock, SourceResolution, encode_source_lock};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stone.glu");
        fs::write(&path, AUTHORED_WITH_ARCHIVE).unwrap();
        let lock = SourceLock::new(vec![SourceResolution::Archive(ArchiveResolution {
            order: 0,
            url: "https://example.com/source.tar.xz".to_owned(),
            sha256: "b".repeat(64),
        })]);
        fs::write(root.path().join(SOURCE_LOCK_FILE_NAME), encode_source_lock(&lock)).unwrap();

        let error = bump(path.clone(), None).unwrap_err();

        assert!(
            matches!(&error, Error::StaleGeneratedLock { path: lock_path, .. } if lock_path.ends_with(SOURCE_LOCK_FILE_NAME))
        );
        assert!(error.to_string().contains("recipe update"));
        assert_eq!(fs::read_to_string(path).unwrap(), AUTHORED_WITH_ARCHIVE);
    }
}
