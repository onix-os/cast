// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsString};
use std::io::{self, BufReader, Read};
use std::num::NonZeroU64;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use fs_err as fs;
use fs_err::os::unix::fs::OpenOptionsExt;
use stone::{
    StoneDigestWriterHasher,
    relation::{Dependency, ParseError, Provider},
};
use thiserror::Error;

use stone_recipe::{
    derivation::{AnalysisPlan, DerivationId, DerivationPlan, OutputRelation, PackageIdentity, PathRuleKind},
    package::OutputSpec,
};

use crate::{Paths, Recipe, Timing, timing};

use self::collect::Collector;

mod analysis;
mod collect;
mod emit;

const PUBLISHED_ARTEFACT_MODE: u32 = 0o644;
const PUBLISHED_BUNDLE_MODE: u32 = 0o755;

#[cfg(test)]
pub(crate) use emit::test_derivation_plan;

pub struct Packager {
    packages: BTreeMap<String, ResolvedOutput>,
    collector: Collector,
}

/// One emitted package resolved from a direct package-v3 output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResolvedOutput {
    pub(crate) include_in_manifest: bool,
    pub(crate) summary: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) provides_exclude: Vec<String>,
    pub(crate) runtime_inputs: Vec<Dependency>,
    pub(crate) runtime_exclude: Vec<String>,
    pub(crate) conflicts: Vec<Provider>,
}

pub struct FrozenPackager<'a> {
    paths: &'a Paths,
    identity: PackageIdentity,
    packages: BTreeMap<String, ResolvedOutput>,
    collector: Collector,
    build_release: NonZeroU64,
    recipe_fingerprint: String,
    analysis: AnalysisPlan,
    architecture: crate::Architecture,
    manifest_build_inputs: Vec<Dependency>,
    jobs: u32,
    derivation_id: DerivationId,
}

impl<'a> FrozenPackager<'a> {
    pub fn from_plan(paths: &'a Paths, plan: &DerivationPlan) -> Result<Self, Error> {
        plan.validate().map_err(Error::InvalidFrozenPlan)?;
        if paths.layout() != &plan.layout {
            return Err(Error::FrozenLayoutMismatch);
        }
        let output_packages = plan
            .outputs
            .iter()
            .map(|output| (output.name.as_str(), output.package_name.as_str()))
            .collect::<BTreeMap<_, _>>();
        let packages = plan
            .outputs
            .iter()
            .map(|output| {
                let run_deps = output
                    .runtime_inputs
                    .iter()
                    .map(|relation| match relation {
                        OutputRelation::Locked { relation, .. } => Ok(relation.to_dependency()),
                        OutputRelation::Planned { output } => output_packages
                            .get(output.as_str())
                            .map(|package| Dependency::package_name(*package))
                            .ok_or_else(|| Error::MissingFrozenOutput(output.clone())),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((
                    output.package_name.clone(),
                    ResolvedOutput {
                        include_in_manifest: output.include_in_manifest,
                        summary: output.summary.clone(),
                        description: output.description.clone(),
                        provides_exclude: output.provides_exclude.clone(),
                        runtime_inputs: run_deps,
                        runtime_exclude: output.runtime_exclude.clone(),
                        conflicts: output.conflicts.iter().map(|relation| relation.to_provider()).collect(),
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, Error>>()?;

        let mut collector = Collector::new(&plan.layout.install_dir);
        for rule in &plan.collection_rules {
            let package = output_packages
                .get(rule.output.as_str())
                .ok_or_else(|| Error::MissingFrozenOutput(rule.output.clone()))?;
            collector.add_rule(collect::Rule {
                pattern: rule.pattern.clone(),
                package: (*package).to_owned(),
                kind: rule.kind,
            });
        }

        let manifest_build_inputs = plan
            .manifest_build_inputs
            .iter()
            .map(|relation| relation.to_dependency())
            .collect();

        Ok(Self {
            paths,
            identity: plan.package.clone(),
            packages,
            collector,
            build_release: NonZeroU64::new(plan.package.build_release).expect("validated build release"),
            recipe_fingerprint: plan.provenance.recipe.sha256.clone(),
            analysis: plan.analysis.clone(),
            architecture: frozen_architecture(&plan.package.architecture),
            manifest_build_inputs,
            jobs: plan.execution.jobs,
            derivation_id: plan.derivation_id(),
        })
    }

    pub fn package(&self, timing: &mut Timing) -> Result<(), Error> {
        let mut hasher = StoneDigestWriterHasher::new();
        let timer = timing.begin(timing::Kind::Analyze);
        let paths = self
            .collector
            .enumerate_paths(None, &mut hasher)
            .map_err(Error::CollectPaths)?;
        let mut analysis = analysis::Chain::new(self.paths, &self.analysis, &self.collector, &mut hasher);
        analysis.process(paths).map_err(Error::Analysis)?;
        timing.finish(timer);

        let timer = timing.begin(timing::Kind::Emit);
        let packages = self
            .packages
            .iter()
            .map(|(name, package)| {
                let bucket = analysis.buckets.remove(name).unwrap_or_default();
                emit::Package::new_with_architecture(
                    name,
                    &self.identity,
                    package,
                    bucket,
                    self.build_release,
                    self.architecture,
                    self.jobs,
                )
            })
            .collect::<Vec<_>>();
        if let Some(name) = analysis.buckets.keys().next() {
            return Err(Error::UnexpectedAnalyzedOutput(name.clone()));
        }
        emit::emit_frozen(
            self.paths,
            &self.identity,
            &self.recipe_fingerprint,
            self.manifest_build_inputs.clone(),
            self.architecture,
            &packages,
            &self.derivation_id,
        )
        .map_err(Error::Emit)?;
        timing.finish(timer);
        Ok(())
    }
}

fn frozen_architecture(value: &str) -> crate::Architecture {
    match value {
        "x86_64" => crate::Architecture::X86_64,
        "x86" => crate::Architecture::X86,
        "aarch64" => crate::Architecture::Aarch64,
        "riscv64" => crate::Architecture::Riscv64,
        _ => unreachable!("artifact architecture was validated before freeze"),
    }
}

fn stone_artefact_filename(
    package_name: &str,
    version: &str,
    source_release: u64,
    build_release: u64,
    architecture: crate::Architecture,
) -> String {
    format!("{package_name}-{version}-{source_release}-{build_release}-{architecture}.stone")
}

fn binary_manifest_filename(architecture: crate::Architecture) -> String {
    format!("manifest.{architecture}.bin")
}

fn jsonc_manifest_filename(architecture: crate::Architecture) -> String {
    format!("manifest.{architecture}.jsonc")
}

impl Packager {
    pub fn new(paths: &Paths, recipe: &Recipe) -> Result<Self, Error> {
        let mut collector = Collector::new(paths.install().guest);
        let packages = resolve_packages(recipe, &mut collector)?;

        Ok(Self { collector, packages })
    }

    pub(crate) fn resolved_packages(&self) -> &BTreeMap<String, ResolvedOutput> {
        &self.packages
    }

    pub(crate) fn collection_rules(&self) -> impl Iterator<Item = (&str, PathRuleKind, &str)> {
        self.collector
            .rules()
            .iter()
            .map(|rule| (rule.package.as_str(), rule.kind, rule.pattern.as_str()))
    }
}

/// Resolve the concrete typed outputs returned by the Gluon package factory.
fn resolve_packages(recipe: &Recipe, collector: &mut Collector) -> Result<BTreeMap<String, ResolvedOutput>, Error> {
    let mut packages = BTreeMap::new();
    for (index, output) in recipe.declaration.outputs.iter().enumerate() {
        let name = emitted_output_name(&recipe.declaration.meta.pname, &output.name);
        let package = resolved_output(output, index)?;
        for path in &output.paths {
            let (kind, pattern) = collection_rule(path);
            collector.add_rule(collect::Rule {
                pattern: pattern.to_owned(),
                package: name.clone(),
                kind,
            });
        }
        packages.insert(name, package);
    }

    Ok(packages)
}

fn emitted_output_name(pname: &str, output: &str) -> String {
    if output == "out" {
        pname.to_owned()
    } else {
        format!("{pname}-{output}")
    }
}

fn resolved_output(output: &OutputSpec, output_index: usize) -> Result<ResolvedOutput, Error> {
    Ok(ResolvedOutput {
        include_in_manifest: output.include_in_manifest,
        summary: output.summary.clone(),
        description: output.description.clone(),
        provides_exclude: output.provides_exclude.clone(),
        runtime_inputs: output
            .runtime_inputs
            .iter()
            .enumerate()
            .map(|(index, dependency)| {
                dependency.dependency().map_err(|source| Error::InvalidDependency {
                    field: format!("outputs[{output_index}].runtime_inputs[{index}]"),
                    value: format!("{dependency:?}"),
                    source,
                })
            })
            .collect::<Result<_, _>>()?,
        runtime_exclude: output.runtime_exclude.clone(),
        conflicts: output
            .conflicts
            .iter()
            .enumerate()
            .map(|(index, provider)| {
                provider.provider().map_err(|source| Error::InvalidProvider {
                    field: format!("outputs[{output_index}].conflicts[{index}]"),
                    value: format!("{provider:?}"),
                    source,
                })
            })
            .collect::<Result<_, _>>()?,
    })
}

fn collection_rule(path: &stone_recipe::PathSpec) -> (PathRuleKind, &str) {
    match path {
        stone_recipe::PathSpec::Any { path } => (PathRuleKind::Any, path),
        stone_recipe::PathSpec::Exe { path } => (PathRuleKind::Executable, path),
        stone_recipe::PathSpec::Symlink { path } => (PathRuleKind::Symlink, path),
        stone_recipe::PathSpec::Special { path } => (PathRuleKind::Special, path),
    }
}

/// Result of publishing one complete frozen derivation bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Publication {
    /// A new bundle became visible with one atomic rename.
    Published,
    /// The exact byte-identical bundle was already present.
    Reused,
}

/// Publish all emitted artefacts as one immutable, derivation-owned bundle.
///
/// The final path is `<output>/<derivation-id>/`. A new bundle is assembled in
/// a sibling temporary directory and installed with Linux `RENAME_NOREPLACE`,
/// so no observer can see a partial final bundle and an existing bundle is
/// never replaced. The frozen plan defines the complete allowed file set;
/// published regular files are normalized to mode 0644 and the bundle
/// directory to 0755. Re-publishing the same derivation succeeds only when the
/// complete file set, bytes, and normalized modes match.
pub fn publish_artefacts(paths: &Paths, plan: &DerivationPlan) -> Result<Publication, PublishError> {
    plan.validate().map_err(PublishError::InvalidFrozenPlan)?;
    paths.require_plan(plan).map_err(PublishError::InvalidFrozenPaths)?;
    let derivation_id = plan.derivation_id();
    let expected = expected_bundle_files(plan);
    let staged_root = paths.artefacts().host;
    let staged = regular_bundle_files(&staged_root, "staged", None)?;
    require_expected_file_set("staged", &staged_root, &expected, &staged)?;
    let final_root = paths.output_dir().join(derivation_id.as_str());

    if final_root_exists(&final_root)? {
        verify_existing_bundle(&expected, &staged, &final_root)?;
        return Ok(Publication::Reused);
    }

    let temporary = tempfile::Builder::new()
        .prefix(&format!(".{derivation_id}.tmp-"))
        .tempdir_in(paths.output_dir())
        .map_err(|source| PublishError::CreateTemporary {
            output: paths.output_dir().clone(),
            source,
        })?;

    for (name, source) in &staged {
        copy_regular_file(source, &temporary.path().join(name))?;
    }
    fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(PUBLISHED_BUNDLE_MODE)).map_err(
        |source| PublishError::NormalizeMode {
            role: "temporary bundle",
            path: temporary.path().to_owned(),
            mode: PUBLISHED_BUNDLE_MODE,
            source,
        },
    )?;
    sync_directory(temporary.path(), "temporary")?;

    match rename_noreplace(temporary.path(), &final_root) {
        Ok(()) => {
            // The TempDir's former path no longer exists, so dropping it cannot
            // remove the atomically published directory.
            drop(temporary);
            sync_directory(paths.output_dir(), "output")?;
            Ok(Publication::Published)
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            // Another publisher won the race. Its result is reusable only when
            // it is exactly the bundle this derivation would have published.
            verify_existing_bundle(&expected, &staged, &final_root)?;
            Ok(Publication::Reused)
        }
        Err(source) => Err(PublishError::Install {
            temporary: temporary.path().to_owned(),
            final_path: final_root,
            source,
        }),
    }
}

fn expected_bundle_files(plan: &DerivationPlan) -> BTreeSet<OsString> {
    let architecture = frozen_architecture(&plan.package.architecture);
    let mut expected = plan
        .outputs
        .iter()
        .map(|output| {
            OsString::from(stone_artefact_filename(
                &output.package_name,
                &plan.package.version,
                plan.package.source_release,
                plan.package.build_release,
                architecture,
            ))
        })
        .collect::<BTreeSet<_>>();
    expected.insert(OsString::from(binary_manifest_filename(architecture)));
    expected.insert(OsString::from(jsonc_manifest_filename(architecture)));
    expected
}

fn final_root_exists(path: &Path) -> Result<bool, PublishError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            require_mode("published bundle", path, &metadata, PUBLISHED_BUNDLE_MODE)?;
            Ok(true)
        }
        Ok(_) => Err(PublishError::UnexpectedRoot {
            role: "published",
            path: path.to_owned(),
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(PublishError::Inspect {
            role: "published",
            path: path.to_owned(),
            source,
        }),
    }
}

fn regular_bundle_files(
    root: &Path,
    role: &'static str,
    expected_root_mode: Option<u32>,
) -> Result<BTreeMap<OsString, PathBuf>, PublishError> {
    let metadata = fs::symlink_metadata(root).map_err(|source| PublishError::Inspect {
        role,
        path: root.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_dir() {
        return Err(PublishError::UnexpectedRoot {
            role,
            path: root.to_owned(),
        });
    }
    if let Some(expected) = expected_root_mode {
        require_mode(role, root, &metadata, expected)?;
    }

    let entries = fs::read_dir(root).map_err(|source| PublishError::Inspect {
        role,
        path: root.to_owned(),
        source,
    })?;
    let mut files = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|source| PublishError::Inspect {
            role,
            path: root.to_owned(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| PublishError::Inspect {
            role,
            path: path.clone(),
            source,
        })?;
        if !file_type.is_file() {
            return Err(PublishError::UnexpectedEntry { role, path });
        }
        files.insert(entry.file_name(), path);
    }
    Ok(files)
}

fn require_expected_file_set(
    role: &'static str,
    root: &Path,
    expected: &BTreeSet<OsString>,
    files: &BTreeMap<OsString, PathBuf>,
) -> Result<(), PublishError> {
    let found = files.keys().cloned().collect::<BTreeSet<_>>();
    if &found == expected {
        Ok(())
    } else {
        Err(PublishError::FrozenFileSetMismatch {
            role,
            path: root.to_owned(),
            expected: expected.iter().cloned().collect(),
            found: found.into_iter().collect(),
        })
    }
}

fn verify_existing_bundle(
    expected: &BTreeSet<OsString>,
    staged: &BTreeMap<OsString, PathBuf>,
    final_root: &Path,
) -> Result<(), PublishError> {
    let published = regular_bundle_files(final_root, "published bundle", Some(PUBLISHED_BUNDLE_MODE))?;
    require_expected_file_set("published", final_root, expected, &published)?;

    for (name, staged_path) in staged {
        let published_path = &published[name];
        if !regular_files_equal(staged_path, published_path)? {
            return Err(PublishError::ContentMismatch {
                staged: staged_path.clone(),
                published: published_path.clone(),
            });
        }
    }
    Ok(())
}

fn require_mode(
    role: &'static str,
    path: &Path,
    metadata: &std::fs::Metadata,
    expected: u32,
) -> Result<(), PublishError> {
    let found = metadata.permissions().mode() & 0o7777;
    if found == expected {
        Ok(())
    } else {
        Err(PublishError::ModeMismatch {
            role,
            path: path.to_owned(),
            expected,
            found,
        })
    }
}

fn open_regular_file(path: &Path, role: &'static str) -> Result<fs::File, PublishError> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| PublishError::Inspect {
            role,
            path: path.to_owned(),
            source,
        })?;
    if !file
        .metadata()
        .map_err(|source| PublishError::Inspect {
            role,
            path: path.to_owned(),
            source,
        })?
        .file_type()
        .is_file()
    {
        return Err(PublishError::UnexpectedEntry {
            role,
            path: path.to_owned(),
        });
    }
    Ok(file)
}

fn copy_regular_file(source: &Path, target: &Path) -> Result<(), PublishError> {
    let mut input = open_regular_file(source, "staged")?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PUBLISHED_ARTEFACT_MODE)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(target)
        .map_err(|source_error| PublishError::Copy {
            staged: source.to_owned(),
            temporary: target.to_owned(),
            source: source_error,
        })?;
    io::copy(&mut input, &mut output).map_err(|source_error| PublishError::Copy {
        staged: source.to_owned(),
        temporary: target.to_owned(),
        source: source_error,
    })?;
    output
        .set_permissions(std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE))
        .and_then(|()| output.sync_all())
        .map_err(|source_error| PublishError::Copy {
            staged: source.to_owned(),
            temporary: target.to_owned(),
            source: source_error,
        })
}

fn regular_files_equal(staged: &Path, published: &Path) -> Result<bool, PublishError> {
    let staged_file = open_regular_file(staged, "staged")?;
    let published_file = open_regular_file(published, "published")?;
    // Staging modes are deliberately non-semantic: a new publication
    // normalizes them. Reuse must nevertheless prove that the already-visible
    // result retained the frozen publication mode.
    let published_mode = published_file
        .metadata()
        .map_err(|source| PublishError::Inspect {
            role: "published",
            path: published.to_owned(),
            source,
        })?
        .permissions()
        .mode()
        & 0o7777;
    if published_mode != PUBLISHED_ARTEFACT_MODE {
        return Err(PublishError::ModeMismatch {
            role: "published",
            path: published.to_owned(),
            expected: PUBLISHED_ARTEFACT_MODE,
            found: published_mode,
        });
    }
    let staged_len = staged_file
        .metadata()
        .map_err(|source| PublishError::Inspect {
            role: "staged",
            path: staged.to_owned(),
            source,
        })?
        .len();
    let published_len = published_file
        .metadata()
        .map_err(|source| PublishError::Inspect {
            role: "published",
            path: published.to_owned(),
            source,
        })?
        .len();
    if staged_len != published_len {
        return Ok(false);
    }

    let mut staged_reader = BufReader::new(staged_file);
    let mut published_reader = BufReader::new(published_file);
    let mut staged_buffer = [0_u8; 64 * 1024];
    let mut published_buffer = [0_u8; 64 * 1024];
    let mut remaining = staged_len;
    while remaining > 0 {
        let chunk = remaining.min(staged_buffer.len() as u64) as usize;
        staged_reader
            .read_exact(&mut staged_buffer[..chunk])
            .map_err(|source| PublishError::Read {
                path: staged.to_owned(),
                source,
            })?;
        published_reader
            .read_exact(&mut published_buffer[..chunk])
            .map_err(|source| PublishError::Read {
                path: published.to_owned(),
                source,
            })?;
        if staged_buffer[..chunk] != published_buffer[..chunk] {
            return Ok(false);
        }
        remaining -= chunk as u64;
    }

    // Detect a file that grew after its metadata was sampled.
    let mut byte = [0_u8; 1];
    Ok(staged_reader.read(&mut byte).map_err(|source| PublishError::Read {
        path: staged.to_owned(),
        source,
    })? == 0
        && published_reader.read(&mut byte).map_err(|source| PublishError::Read {
            path: published.to_owned(),
            source,
        })? == 0)
}

fn sync_directory(path: &Path, role: &'static str) -> Result<(), PublishError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| PublishError::SyncDirectory {
            role,
            path: path.to_owned(),
            source,
        })
}

fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "temporary bundle path contains NUL"))?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "final bundle path contains NUL"))?;
    // nix 0.27 exposes renameat2 only for glibc targets. Cast also builds
    // for musl, so invoke the Linux syscall directly with RENAME_NOREPLACE.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            nix::libc::AT_FDCWD,
            source.as_ptr(),
            nix::libc::AT_FDCWD,
            target.as_ptr(),
            1_u32, // RENAME_NOREPLACE
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum PublishError {
    #[error("inspect {role} artefact path {path:?}")]
    Inspect {
        role: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("frozen artefact paths are not bound to the published derivation")]
    InvalidFrozenPaths(#[source] io::Error),
    #[error("invalid frozen derivation plan")]
    InvalidFrozenPlan(#[source] stone_recipe::derivation::DerivationValidationError),
    #[error("{role} artefact root {path:?} must be a real directory")]
    UnexpectedRoot { role: &'static str, path: PathBuf },
    #[error("{role} artefact entry {path:?} must be a direct regular file")]
    UnexpectedEntry { role: &'static str, path: PathBuf },
    #[error("create sibling temporary bundle in {output:?}")]
    CreateTemporary {
        output: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("copy staged artefact {staged:?} to temporary bundle entry {temporary:?}")]
    Copy {
        staged: PathBuf,
        temporary: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{role} artefact bundle {path:?} does not match the frozen plan (expected {expected:?}, found {found:?})")]
    FrozenFileSetMismatch {
        role: &'static str,
        path: PathBuf,
        expected: Vec<OsString>,
        found: Vec<OsString>,
    },
    #[error("{role} path {path:?} has mode {found:#06o}; expected reproducible mode {expected:#06o}")]
    ModeMismatch {
        role: &'static str,
        path: PathBuf,
        expected: u32,
        found: u32,
    },
    #[error("set {role} path {path:?} to reproducible mode {mode:#06o}")]
    NormalizeMode {
        role: &'static str,
        path: PathBuf,
        mode: u32,
        #[source]
        source: io::Error,
    },
    #[error("published artefact {published:?} does not match staged bytes from {staged:?}")]
    ContentMismatch { staged: PathBuf, published: PathBuf },
    #[error("read artefact file {path:?}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("sync {role} bundle directory {path:?}")]
    SyncDirectory {
        role: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("atomically install temporary derivation bundle {temporary:?} at {final_path:?}")]
    Install {
        temporary: PathBuf,
        final_path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("collect install paths")]
    CollectPaths(#[source] collect::Error),
    #[error("analyzing paths")]
    Analysis(#[source] analysis::BoxError),
    #[error("emit packages")]
    Emit(#[from] emit::Error),
    #[error("{field}: invalid dependency `{value}`")]
    InvalidDependency {
        field: String,
        value: String,
        #[source]
        source: ParseError,
    },
    #[error("{field}: invalid provider `{value}`")]
    InvalidProvider {
        field: String,
        value: String,
        #[source]
        source: ParseError,
    },
    #[error("invalid frozen derivation plan")]
    InvalidFrozenPlan(#[source] stone_recipe::derivation::DerivationValidationError),
    #[error("frozen output {0} is missing")]
    MissingFrozenOutput(String),
    #[error("analysis produced undeclared output {0}")]
    UnexpectedAnalyzedOutput(String),
    #[error("frozen derivation layout does not match runtime paths")]
    FrozenLayoutMismatch,
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::Path;

    use super::*;
    use stone_recipe::build_policy::AnalyzerKind;
    use stone_recipe::derivation::{
        AnalysisToolsPlan, CollectionRulePlan, ExecutablePlan, OutputPlan, PathRuleKind, RelationKind, RelationPlan,
    };

    fn frozen_analyzer_tool(name: &str) -> ExecutablePlan {
        ExecutablePlan {
            path: format!("/usr/bin/{name}"),
            requirement: RelationPlan {
                kind: RelationKind::Binary,
                name: name.to_owned(),
            },
        }
    }

    #[test]
    fn package_factory_defaults_resolve_directly() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let install = tempfile::tempdir().unwrap();
        let mut collector = Collector::new(install.path());

        let packages = resolve_packages(&recipe, &mut collector).unwrap();

        // Golden split policy is now returned as typed values by mk_package.
        assert_eq!(
            packages.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "hello",
                "hello-32bit",
                "hello-32bit-dbginfo",
                "hello-32bit-devel",
                "hello-dbginfo",
                "hello-demos",
                "hello-devel",
                "hello-docs",
                "hello-libs",
            ]
        );
        let rules = collector.rules();
        assert_eq!(
            rules.last().map(|rule| (rule.package.as_str(), rule.pattern.as_str())),
            Some(("hello-demos", "/usr/lib/qt*/examples"))
        );
        assert_ne!(
            rules.last().map(|rule| rule.package.as_str()),
            packages.keys().last().map(String::as_str),
            "collector precedence must retain composition order rather than package-map order"
        );

        let root = &packages["hello"];
        assert_eq!(root.summary.as_deref(), Some("Minimal Gluon recipe example"));
        assert!(root.include_in_manifest);
        assert_eq!(
            rules
                .iter()
                .filter(|rule| rule.package == "hello")
                .map(|rule| (rule.kind, rule.pattern.as_str()))
                .collect::<Vec<_>>(),
            [(PathRuleKind::Any, "*")]
        );
        assert!(!packages["hello-dbginfo"].include_in_manifest);
        assert!(!packages["hello-32bit-dbginfo"].include_in_manifest);

        let devel = &packages["hello-devel"];
        assert_eq!(devel.summary.as_deref(), Some("Development files for hello"));
        assert_eq!(
            devel.description.as_deref(),
            Some("Install this package if you intend to build software against\nthe hello package.")
        );
        assert_eq!(
            devel.runtime_inputs.iter().map(Dependency::to_name).collect::<Vec<_>>(),
            ["hello"]
        );
        assert_eq!(
            rules
                .iter()
                .filter(|rule| rule.package == "hello-devel")
                .map(|rule| rule.pattern.as_str())
                .collect::<Vec<_>>(),
            [
                "/usr/include",
                "/usr/lib/*.a",
                "/usr/lib/cmake",
                "/usr/lib/lib*.so",
                "/usr/lib/pkgconfig",
                "/usr/share/aclocal",
                "/usr/share/cmake",
                "/usr/share/man/man2",
                "/usr/share/man/man3",
                "/usr/share/man/man9",
                "/usr/share/pkgconfig",
                "/usr/share/gir-1.0/*.gir",
                "/usr/share/vala/vapi/*.deps",
                "/usr/share/vala/vapi/*.vapi",
                "/usr/lib/*.prl",
                "/usr/lib/metatypes",
                "/usr/lib/qt*/metatypes/qt*.json",
                "/usr/lib/qt*/mkspecs",
                "/usr/lib/qt*/modules/*.json",
                "/usr/lib/qt*/sbom",
                "/usr/lib/qt*/plugins/designer/*.so",
                "/usr/share/doc/qt5/*.qch",
                "/usr/share/doc/qt5/*.tags",
                "/usr/share/doc/qt6/*.qch",
                "/usr/share/doc/qt6/*.tags",
            ]
        );
    }

    #[test]
    fn resolved_outputs_do_not_inherit_root_metadata() {
        let mut recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let root = recipe
            .declaration
            .outputs
            .iter_mut()
            .find(|output| output.name == "out")
            .unwrap();
        root.summary = Some("Root summary only".to_owned());
        root.description = Some("Root description only".to_owned());
        let split = recipe
            .declaration
            .outputs
            .iter_mut()
            .find(|output| output.name == "libs")
            .unwrap();
        split.summary = None;
        split.description = None;

        let install = tempfile::tempdir().unwrap();
        let mut collector = Collector::new(install.path());
        let packages = resolve_packages(&recipe, &mut collector).unwrap();

        assert_eq!(packages["hello"].summary.as_deref(), Some("Root summary only"));
        assert_eq!(packages["hello"].description.as_deref(), Some("Root description only"));
        assert_eq!(packages["hello-libs"].summary, None);
        assert_eq!(packages["hello-libs"].description, None);
    }

    #[test]
    fn frozen_packager_uses_only_plan_outputs_rules_analysis_and_identity() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        plan.package.name = "frozen".to_owned();
        plan.package.homepage = "https://frozen.invalid".to_owned();
        plan.package.architecture = "x86".to_owned();
        plan.build_lock.target_platform.architecture = "x86".to_owned();
        plan.package.licenses = vec!["MIT".to_owned()];
        plan.analysis = AnalysisPlan {
            handlers: vec![
                AnalyzerKind::IgnoreBlocked,
                AnalyzerKind::Binary,
                AnalyzerKind::Elf,
                AnalyzerKind::PkgConfig,
                AnalyzerKind::Python,
                AnalyzerKind::CMake,
                AnalyzerKind::CompressMan,
                AnalyzerKind::IncludeAny,
            ],
            tools: AnalysisToolsPlan {
                pkg_config: Some(frozen_analyzer_tool("pkg-config")),
                python: Some(frozen_analyzer_tool("python3")),
                objcopy: Some(frozen_analyzer_tool("objcopy")),
                strip: None,
            },
            debug: true,
            strip: false,
            compress_man: false,
            remove_libtool: false,
        };
        plan.manifest_build_inputs = vec![RelationPlan {
            kind: RelationKind::Binary,
            name: "frozen-build-input".to_owned(),
        }];
        plan.outputs = vec![OutputPlan {
            name: "out".to_owned(),
            package_name: "frozen".to_owned(),
            include_in_manifest: true,
            summary: Some("Frozen output".to_owned()),
            description: Some("Only plan data".to_owned()),
            provides_exclude: vec!["excluded-provider".to_owned()],
            runtime_exclude: vec!["excluded-runtime".to_owned()],
            runtime_inputs: Vec::new(),
            conflicts: vec![RelationPlan {
                kind: RelationKind::PkgConfig,
                name: "conflict".to_owned(),
            }],
        }];
        plan.collection_rules = vec![
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Any,
                pattern: "*".to_owned(),
            },
            CollectionRulePlan {
                output: "out".to_owned(),
                kind: PathRuleKind::Executable,
                pattern: "/usr/bin/*".to_owned(),
            },
        ];
        plan.validate().unwrap();
        let expected_id = plan.derivation_id();

        let packager = FrozenPackager::from_plan(&paths, &plan).unwrap();
        assert_eq!(packager.identity.name, "frozen");
        assert_eq!(packager.identity.homepage, "https://frozen.invalid");
        assert_eq!(packager.architecture, crate::Architecture::X86);
        assert_eq!(packager.analysis, plan.analysis);
        assert_eq!(packager.recipe_fingerprint, plan.provenance.recipe.sha256);
        assert_eq!(
            packager
                .manifest_build_inputs
                .iter()
                .map(Dependency::to_name)
                .collect::<Vec<_>>(),
            ["binary(frozen-build-input)"]
        );
        assert_eq!(packager.derivation_id, expected_id);
        assert_eq!(
            packager
                .collector
                .rules()
                .iter()
                .map(|rule| (rule.package.as_str(), rule.kind, rule.pattern.as_str()))
                .collect::<Vec<_>>(),
            [
                ("frozen", PathRuleKind::Any, "*"),
                ("frozen", PathRuleKind::Executable, "/usr/bin/*"),
            ]
        );
        let output = &packager.packages["frozen"];
        assert_eq!(output.summary.as_deref(), Some("Frozen output"));
        assert_eq!(
            output.conflicts.iter().map(Provider::to_name).collect::<Vec<_>>(),
            ["pkgconfig(conflict)"]
        );
    }

    #[test]
    fn frozen_packager_rejects_runtime_and_plan_layout_mismatch() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        plan.layout.hostname = "different-builder".to_owned();
        plan.validate().unwrap();

        assert!(matches!(
            FrozenPackager::from_plan(&paths, &plan),
            Err(Error::FrozenLayoutMismatch)
        ));
    }

    fn publication_fixture() -> (tempfile::TempDir, DerivationPlan, Paths) {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        fs::create_dir(&output).unwrap();
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let plan = test_derivation_plan();
        let mut paths = Paths::new(&recipe, plan.layout.clone(), root.path(), output).unwrap();
        paths.bind_to_plan(&plan).unwrap();
        (root, plan, paths)
    }

    fn output_entries(paths: &Paths) -> Vec<OsString> {
        let mut entries = fs::read_dir(paths.output_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    fn stage_expected_bundle(plan: &DerivationPlan, paths: &Paths) -> Vec<OsString> {
        let names = expected_bundle_files(plan).into_iter().collect::<Vec<_>>();
        for name in &names {
            let path = paths.artefacts().host.join(name);
            fs::write(&path, b"frozen artefact bytes").unwrap();
            fs::set_permissions(path, std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE)).unwrap();
        }
        names
    }

    fn stone_name(names: &[OsString]) -> &OsString {
        names
            .iter()
            .find(|name| name.to_string_lossy().ends_with(".stone"))
            .unwrap()
    }

    #[test]
    fn frozen_bundle_contract_names_every_declared_output_and_both_manifests() {
        let mut plan = test_derivation_plan();
        plan.outputs.push(OutputPlan {
            name: "dev".to_owned(),
            package_name: "example-devel".to_owned(),
            include_in_manifest: true,
            summary: None,
            description: None,
            provides_exclude: Vec::new(),
            runtime_exclude: Vec::new(),
            runtime_inputs: Vec::new(),
            conflicts: Vec::new(),
        });
        plan.validate().unwrap();

        assert_eq!(
            expected_bundle_files(&plan).into_iter().collect::<Vec<_>>(),
            [
                OsString::from("example-1.2.3-1-1-x86_64.stone"),
                OsString::from("example-devel-1.2.3-1-1-x86_64.stone"),
                OsString::from("manifest.x86_64.bin"),
                OsString::from("manifest.x86_64.jsonc"),
            ]
        );
    }

    #[test]
    fn publishes_and_reuses_one_complete_derivation_bundle() {
        let (_root, plan, paths) = publication_fixture();
        let staged = paths.artefacts().host;
        let names = stage_expected_bundle(&plan, &paths);
        assert_eq!(
            names,
            [
                OsString::from("example-1.2.3-1-1-x86_64.stone"),
                OsString::from("manifest.x86_64.bin"),
                OsString::from("manifest.x86_64.jsonc"),
            ]
        );
        let package = staged.join(stone_name(&names));

        assert_eq!(publish_artefacts(&paths, &plan).unwrap(), Publication::Published);

        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        assert_eq!(
            fs::metadata(&bundle).unwrap().permissions().mode() & 0o7777,
            PUBLISHED_BUNDLE_MODE
        );
        for name in &names {
            assert_eq!(fs::read(bundle.join(name)).unwrap(), b"frozen artefact bytes");
            assert_eq!(
                fs::metadata(bundle.join(name)).unwrap().permissions().mode() & 0o7777,
                PUBLISHED_ARTEFACT_MODE
            );
        }
        assert_ne!(
            fs::metadata(&package).unwrap().ino(),
            fs::metadata(bundle.join(stone_name(&names))).unwrap().ino(),
            "published files must not retain mutable staging inodes"
        );
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
        assert!(!paths.output_dir().join(stone_name(&names)).exists());

        assert_eq!(publish_artefacts(&paths, &plan).unwrap(), Publication::Reused);
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
    }

    #[test]
    fn mismatched_existing_bundle_is_never_modified() {
        let (_root, plan, paths) = publication_fixture();
        let staged = paths.artefacts().host;
        let names = stage_expected_bundle(&plan, &paths);
        let package_name = stone_name(&names);
        publish_artefacts(&paths, &plan).unwrap();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());

        fs::write(staged.join(package_name), b"different").unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(error, PublishError::ContentMismatch { .. }));
        assert_eq!(fs::read(bundle.join(package_name)).unwrap(), b"frozen artefact bytes");

        fs::write(staged.join(package_name), b"frozen artefact bytes").unwrap();
        fs::write(staged.join("extra.stone"), b"extra").unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::FrozenFileSetMismatch { role: "staged", .. }
        ));
        assert!(!bundle.join("extra.stone").exists());
        assert_eq!(output_entries(&paths), [OsString::from(plan.derivation_id().as_str())]);
    }

    #[test]
    fn missing_or_extra_staged_files_are_rejected_before_publication() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let staged = paths.artefacts().host;
        let missing = names[0].clone();
        fs::remove_file(staged.join(&missing)).unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            &error,
            PublishError::FrozenFileSetMismatch {
                role: "staged",
                expected,
                found,
                ..
            } if expected.contains(&missing) && !found.contains(&missing)
        ));
        assert!(output_entries(&paths).is_empty());

        fs::write(staged.join(&missing), b"frozen artefact bytes").unwrap();
        fs::set_permissions(
            staged.join(&missing),
            std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE),
        )
        .unwrap();
        let extra = OsString::from("undeclared-debug-output.stone");
        fs::write(staged.join(&extra), b"undeclared").unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            &error,
            PublishError::FrozenFileSetMismatch {
                role: "staged",
                expected,
                found,
                ..
            } if !expected.contains(&extra) && found.contains(&extra)
        ));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn wrong_staged_modes_are_normalized_to_reproducible_0644() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let staged_path = paths.artefacts().host.join(&names[0]);
        fs::set_permissions(&staged_path, std::fs::Permissions::from_mode(0o6755)).unwrap();

        publish_artefacts(&paths, &plan).unwrap();

        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        assert_eq!(fs::metadata(staged_path).unwrap().permissions().mode() & 0o7777, 0o6755);
        for name in names {
            assert_eq!(
                fs::metadata(bundle.join(name)).unwrap().permissions().mode() & 0o7777,
                PUBLISHED_ARTEFACT_MODE
            );
        }
    }

    #[test]
    fn existing_bundle_mode_mismatch_is_never_reused() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        publish_artefacts(&paths, &plan).unwrap();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        let published = bundle.join(&names[0]);
        fs::set_permissions(&published, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::ModeMismatch {
                role: "published",
                expected: PUBLISHED_ARTEFACT_MODE,
                found: 0o600,
                ..
            }
        ));
        assert_eq!(fs::metadata(published).unwrap().permissions().mode() & 0o7777, 0o600);
    }

    #[test]
    fn existing_bundle_directory_mode_mismatch_is_never_reused() {
        let (_root, plan, paths) = publication_fixture();
        stage_expected_bundle(&plan, &paths);
        publish_artefacts(&paths, &plan).unwrap();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            error,
            PublishError::ModeMismatch {
                role: "published bundle",
                expected: PUBLISHED_BUNDLE_MODE,
                found: 0o700,
                ..
            }
        ));
        assert_eq!(fs::metadata(bundle).unwrap().permissions().mode() & 0o7777, 0o700);
    }

    #[test]
    fn existing_bundle_file_set_must_still_match_the_frozen_plan() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        publish_artefacts(&paths, &plan).unwrap();
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        let missing = names[0].clone();
        fs::remove_file(bundle.join(&missing)).unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            &error,
            PublishError::FrozenFileSetMismatch {
                role: "published",
                expected,
                found,
                ..
            } if expected.contains(&missing) && !found.contains(&missing)
        ));

        fs::write(bundle.join(&missing), b"frozen artefact bytes").unwrap();
        fs::set_permissions(
            bundle.join(&missing),
            std::fs::Permissions::from_mode(PUBLISHED_ARTEFACT_MODE),
        )
        .unwrap();
        let extra = OsString::from("undeclared-published-file");
        fs::write(bundle.join(&extra), b"extra").unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(
            &error,
            PublishError::FrozenFileSetMismatch {
                role: "published",
                expected,
                found,
                ..
            } if !expected.contains(&extra) && found.contains(&extra)
        ));
    }

    #[test]
    fn rejects_non_regular_or_nested_staged_entries_without_a_final_bundle() {
        let (_root, plan, paths) = publication_fixture();
        let staged = paths.artefacts().host;
        fs::create_dir(staged.join("nested")).unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(error, PublishError::UnexpectedEntry { .. }));
        assert!(output_entries(&paths).is_empty());

        fs::remove_dir(staged.join("nested")).unwrap();
        fs::write(staged.join("target"), b"bytes").unwrap();
        symlink("target", staged.join("link.stone")).unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(error, PublishError::UnexpectedEntry { .. }));
        assert!(output_entries(&paths).is_empty());

        fs::remove_file(staged.join("link.stone")).unwrap();
        nix::unistd::mkfifo(
            &staged.join("fifo.stone"),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(error, PublishError::UnexpectedEntry { .. }));
        assert!(output_entries(&paths).is_empty());
    }

    #[test]
    fn rejects_unexpected_entries_in_an_existing_final_bundle() {
        let (_root, plan, paths) = publication_fixture();
        let names = stage_expected_bundle(&plan, &paths);
        let package_name = stone_name(&names);
        let bundle = paths.output_dir().join(plan.derivation_id().as_str());
        fs::create_dir(&bundle).unwrap();
        fs::set_permissions(&bundle, std::fs::Permissions::from_mode(PUBLISHED_BUNDLE_MODE)).unwrap();
        symlink("missing", bundle.join(package_name)).unwrap();

        let error = publish_artefacts(&paths, &plan).unwrap_err();
        assert!(matches!(error, PublishError::UnexpectedEntry { .. }));
        assert!(
            fs::symlink_metadata(bundle.join(package_name))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn rename_noreplace_does_not_replace_even_an_empty_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(source.join("complete"), b"bundle").unwrap();

        let error = rename_noreplace(&source, &target).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(source.join("complete").is_file());
        assert!(target.is_dir());
        assert!(fs::read_dir(target).unwrap().next().is_none());
    }
}
