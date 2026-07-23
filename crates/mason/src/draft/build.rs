// SPDX-FileCopyrightText: 2024 AerynOS Developers
use std::collections::BTreeSet;
use std::{
    fmt,
    io::{self, Read},
    num::NonZeroU64,
    path::Path,
    time::{Duration, Instant},
};

use stone::relation::Dependency;

use super::File;

mod autotools;
mod cargo;
mod cmake;
mod meson;
mod perl;
mod python;
mod ruby;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, thiserror::Error)]
#[error("multiple build systems share the highest confidence {confidence}: {systems:?}")]
struct AmbiguousBuildSystems {
    confidence: u64,
    systems: Vec<System>,
}

struct Candidate {
    system: System,
    confidence: NonZeroU64,
    dependencies: BTreeSet<Dependency>,
}

const MAX_ANALYSIS_FILE_BYTES: u64 = 1024 * 1024;
const MAX_ANALYSIS_FILES: usize = 256;
const MAX_ANALYSIS_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_UNIQUE_DEPENDENCIES: usize = 1024;
const MAX_ANALYSIS_WALL_TIME: Duration = Duration::from_secs(30);

#[derive(Default)]
struct AnalysisBudget {
    files: usize,
    declared_bytes: u64,
    read_bytes: u64,
}

impl AnalysisBudget {
    fn admit_declared(&mut self, bytes: u64) -> Result<(), Error> {
        if bytes > MAX_ANALYSIS_FILE_BYTES {
            return Err(analysis_limit_error("one build metadata file", MAX_ANALYSIS_FILE_BYTES));
        }
        let files = self.files.checked_add(1).ok_or_else(analysis_overflow)?;
        if files > MAX_ANALYSIS_FILES {
            return Err(analysis_limit_error("build metadata files", MAX_ANALYSIS_FILES));
        }
        let declared_bytes = self.declared_bytes.checked_add(bytes).ok_or_else(analysis_overflow)?;
        if declared_bytes > MAX_ANALYSIS_TOTAL_BYTES {
            return Err(analysis_limit_error(
                "aggregate declared build metadata bytes",
                MAX_ANALYSIS_TOTAL_BYTES,
            ));
        }
        self.files = files;
        self.declared_bytes = declared_bytes;
        Ok(())
    }

    fn admit_read(&mut self, bytes: u64) -> Result<(), Error> {
        let read_bytes = self.read_bytes.checked_add(bytes).ok_or_else(analysis_overflow)?;
        if read_bytes > MAX_ANALYSIS_TOTAL_BYTES {
            return Err(analysis_limit_error(
                "aggregate read build metadata bytes",
                MAX_ANALYSIS_TOTAL_BYTES,
            ));
        }
        self.read_bytes = read_bytes;
        Ok(())
    }
}

fn read_analysis_text(path: &Path, budget: &mut AnalysisBudget) -> Result<String, Error> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_ANALYSIS_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ANALYSIS_FILE_BYTES {
        return Err(analysis_limit_error("one build metadata file", MAX_ANALYSIS_FILE_BYTES));
    }
    budget.admit_read(bytes.len() as u64)?;
    Ok(String::from_utf8(bytes)?)
}

fn analysis_limit_error(resource: &'static str, limit: impl fmt::Display) -> Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{resource} exceed limit of {limit}"),
    )
    .into()
}

fn analysis_overflow() -> Error {
    io::Error::new(io::ErrorKind::InvalidData, "draft analysis arithmetic overflow").into()
}

/// A build system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, strum::Display)]
#[strum(serialize_all = "lowercase")]
pub enum System {
    Autotools,
    Cargo,
    Cmake,
    Meson,
    PythonPep517,
    PythonSetupTools,
    RubyGem,
    RubyTarball,
    PerlExtutilsMakefile,
    PerlModuleBuild,
}

impl System {
    const ALL: &'static [Self] = &[
        Self::Autotools,
        Self::Cargo,
        Self::Cmake,
        Self::Meson,
        Self::PythonPep517,
        Self::PythonSetupTools,
        Self::RubyGem,
        Self::RubyTarball,
        Self::PerlExtutilsMakefile,
        Self::PerlModuleBuild,
    ];

    fn process(&self, state: &mut State<'_>, file: &File) -> Result<(), Error> {
        match self {
            System::Autotools => autotools::process(state, file),
            System::Cargo => cargo::process(state, file),
            System::Cmake => cmake::process(state, file),
            System::Meson => meson::process(state, file),
            System::PythonPep517 => python::pep517::process(state, file),
            System::PythonSetupTools => python::setup_tools::process(state, file),
            System::RubyGem => ruby::gemfile::process(state, file),
            System::RubyTarball => ruby::tarball::process(state, file),
            System::PerlExtutilsMakefile => perl::extutils_makefile::process(state, file),
            System::PerlModuleBuild => perl::module_build::process(state, file),
        }
    }
}

/// State passed to each system when processing paths
struct State<'a> {
    /// Any dependencies that need to be recorded
    dependencies: &'a mut BTreeSet<Dependency>,
    /// Total confidence level of the current build [`System`]
    confidence: u64,
    analysis_budget: &'a mut AnalysisBudget,
}

impl State<'_> {
    /// Increase the confidence that this project uses the current build [`System`]
    pub fn increment_confidence(&mut self, amount: u64) {
        self.confidence += amount;
    }

    /// Add a dependency to output in `builddeps`
    pub fn add_dependency(&mut self, dependency: Dependency) -> Result<(), Error> {
        if !self.dependencies.contains(&dependency) && self.dependencies.len() >= MAX_UNIQUE_DEPENDENCIES {
            return Err(analysis_limit_error(
                "unique draft dependencies",
                MAX_UNIQUE_DEPENDENCIES,
            ));
        }
        self.dependencies.insert(dependency);
        Ok(())
    }

    fn read_analysis_text(&mut self, path: &Path) -> Result<String, Error> {
        read_analysis_text(path, self.analysis_budget)
    }
}

/// Analysis results from [`analyze`]
pub struct Analysis {
    /// The detected build [`System`], if any
    pub detected_system: Option<System>,
    /// All detected dependencies
    pub dependencies: BTreeSet<Dependency>,
}

/// Analyze the provided paths to determine which build [`System`]
/// the project uses and any dependencies that are identified
pub fn analyze(files: &[File]) -> Result<Analysis, Error> {
    analyze_with_deadline(files, MAX_ANALYSIS_WALL_TIME)
}

fn analyze_with_deadline(files: &[File], wall_time: Duration) -> Result<Analysis, Error> {
    let started = Instant::now();
    let mut analysis_budget = preflight_analysis_inputs(files, started, wall_time)?;
    let mut candidates = Vec::new();

    for system in System::ALL {
        require_analysis_time(started, wall_time)?;
        let mut dependencies = BTreeSet::new();
        let mut state = State {
            dependencies: &mut dependencies,
            confidence: 0,
            analysis_budget: &mut analysis_budget,
        };

        for path in files {
            require_analysis_time(started, wall_time)?;
            system.process(&mut state, path)?;
            require_analysis_time(started, wall_time)?;
        }

        if let Some(confidence) = NonZeroU64::new(state.confidence) {
            candidates.push(Candidate {
                system: *system,
                confidence,
                dependencies,
            });
        }
    }

    let Some(highest) = candidates.iter().map(|candidate| candidate.confidence).max() else {
        return Ok(Analysis {
            detected_system: None,
            dependencies: BTreeSet::new(),
        });
    };
    let mut winners = candidates
        .into_iter()
        .filter(|candidate| candidate.confidence == highest)
        .collect::<Vec<_>>();
    if winners.len() != 1 {
        return Err(Box::new(AmbiguousBuildSystems {
            confidence: highest.get(),
            systems: winners.iter().map(|candidate| candidate.system).collect(),
        }));
    }
    let winner = winners.pop().expect("one winner was checked");

    Ok(Analysis {
        detected_system: Some(winner.system),
        dependencies: winner.dependencies,
    })
}

fn preflight_analysis_inputs(files: &[File], started: Instant, wall_time: Duration) -> Result<AnalysisBudget, Error> {
    let mut budget = AnalysisBudget::default();
    for file in files.iter().filter(|file| is_read_analysis_input(file)) {
        require_analysis_time(started, wall_time)?;
        let metadata = std::fs::symlink_metadata(&file.path)?;
        if !metadata.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("draft metadata is not a regular file: {}", file.path.display()),
            )
            .into());
        }
        // Count every manifest path, including hardlinks to the same inode.
        budget.admit_declared(metadata.len())?;
    }
    Ok(budget)
}

fn is_read_analysis_input(file: &File) -> bool {
    file.depth() == 0 && matches!(file.file_name(), "configure.ac" | "CMakeLists.txt" | "meson.build")
}

fn require_analysis_time(started: Instant, wall_time: Duration) -> Result<(), Error> {
    if started.elapsed() < wall_time {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("draft build-system analysis exceeded {wall_time:?}"),
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_text_limit_accepts_n_and_rejects_n_plus_one() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("metadata");
        std::fs::write(&path, vec![b'a'; MAX_ANALYSIS_FILE_BYTES as usize]).unwrap();
        let mut budget = AnalysisBudget::default();
        assert_eq!(
            read_analysis_text(&path, &mut budget).unwrap().len(),
            MAX_ANALYSIS_FILE_BYTES as usize
        );

        std::fs::write(&path, vec![b'a'; MAX_ANALYSIS_FILE_BYTES as usize + 1]).unwrap();
        assert!(read_analysis_text(&path, &mut budget).is_err());
    }

    #[test]
    fn expired_analysis_deadline_stops_before_running_analyzers() {
        assert!(analyze_with_deadline(&[], Duration::ZERO).is_err());
    }

    #[test]
    fn metadata_count_and_aggregate_byte_budgets_accept_n_and_reject_n_plus_one() {
        let mut count = AnalysisBudget::default();
        for _ in 0..MAX_ANALYSIS_FILES {
            count.admit_declared(0).unwrap();
        }
        assert!(count.admit_declared(0).is_err());

        let mut declared = AnalysisBudget::default();
        for _ in 0..(MAX_ANALYSIS_TOTAL_BYTES / MAX_ANALYSIS_FILE_BYTES) {
            declared.admit_declared(MAX_ANALYSIS_FILE_BYTES).unwrap();
        }
        assert_eq!(declared.declared_bytes, MAX_ANALYSIS_TOTAL_BYTES);
        assert!(declared.admit_declared(1).is_err());

        let mut read = AnalysisBudget::default();
        read.admit_read(MAX_ANALYSIS_TOTAL_BYTES).unwrap();
        assert!(read.admit_read(1).is_err());
    }

    #[test]
    fn preflight_counts_each_hardlink_path_against_declared_bytes() {
        let root = tempfile::tempdir().unwrap();
        let original_dir = root.path().join("original");
        std::fs::create_dir(&original_dir).unwrap();
        let original = original_dir.join("CMakeLists.txt");
        std::fs::write(&original, vec![b'a'; MAX_ANALYSIS_FILE_BYTES as usize]).unwrap();
        let mut files = vec![File::new(original.clone(), 0)];
        for index in 1..=(MAX_ANALYSIS_TOTAL_BYTES / MAX_ANALYSIS_FILE_BYTES) {
            let directory = root.path().join(format!("link-{index}"));
            std::fs::create_dir(&directory).unwrap();
            let link = directory.join("CMakeLists.txt");
            std::fs::hard_link(&original, &link).unwrap();
            files.push(File::new(link, 0));
        }

        assert!(preflight_analysis_inputs(&files[..files.len() - 1], Instant::now(), Duration::from_secs(1)).is_ok());
        assert!(preflight_analysis_inputs(&files, Instant::now(), Duration::from_secs(1)).is_err());
    }

    #[test]
    fn unique_dependency_budget_accepts_n_duplicate_and_rejects_new_n_plus_one() {
        let mut dependencies = BTreeSet::new();
        let mut analysis_budget = AnalysisBudget::default();
        let mut state = State {
            dependencies: &mut dependencies,
            confidence: 0,
            analysis_budget: &mut analysis_budget,
        };
        for index in 0..MAX_UNIQUE_DEPENDENCIES {
            state
                .add_dependency(Dependency {
                    kind: stone::relation::Kind::PackageName,
                    name: format!("dependency-{index}"),
                })
                .unwrap();
        }
        state
            .add_dependency(Dependency {
                kind: stone::relation::Kind::PackageName,
                name: "dependency-0".to_owned(),
            })
            .unwrap();
        assert!(
            state
                .add_dependency(Dependency {
                    kind: stone::relation::Kind::PackageName,
                    name: "dependency-overflow".to_owned(),
                })
                .is_err()
        );
    }

    #[test]
    fn lower_confidence_build_system_dependencies_never_leak_into_winner() {
        let root = tempfile::tempdir().unwrap();
        let cmake = root.path().join("CMakeLists.txt");
        let meson = root.path().join("meson.build");
        std::fs::write(&cmake, "find_package(LosingDependency)").unwrap();
        std::fs::write(&meson, "dependency('winning-dependency')").unwrap();

        let analysis = analyze(&[File::new(cmake, 0), File::new(meson, 0)]).unwrap();

        assert_eq!(analysis.detected_system, Some(System::Meson));
        assert!(
            analysis
                .dependencies
                .iter()
                .any(|dependency| dependency.name == "winning-dependency")
        );
        assert!(
            analysis
                .dependencies
                .iter()
                .all(|dependency| dependency.name != "LosingDependency")
        );
    }

    #[test]
    fn equal_highest_build_system_confidence_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let cargo = root.path().join("Cargo.toml");
        let meson = root.path().join("meson.build");
        std::fs::write(&cargo, "[package]").unwrap();
        std::fs::write(&meson, "project('ambiguous')").unwrap();

        let error = match analyze(&[File::new(cargo, 0), File::new(meson, 0)]) {
            Ok(_) => panic!("equal top confidence must fail closed"),
            Err(error) => error,
        };

        let ambiguous = error.downcast_ref::<AmbiguousBuildSystems>().unwrap();
        assert_eq!(ambiguous.confidence, 100);
        assert_eq!(ambiguous.systems, vec![System::Cargo, System::Meson]);
    }

    #[test]
    fn nested_only_build_markers_produce_no_candidate() {
        let root = tempfile::tempdir().unwrap();
        let files = [
            "Cargo.toml",
            "pyproject.toml",
            "setup.cfg",
            "setup.py",
            "Makefile.PL",
            "Build.PL",
            "meson_options.txt",
            "example.gemspec",
        ]
        .map(|name| {
            let path = root.path().join(name);
            std::fs::write(&path, b"nested marker").unwrap();
            File::new(path, 1)
        });

        let analysis = analyze(&files).unwrap();

        assert_eq!(analysis.detected_system, None);
        assert!(analysis.dependencies.is_empty());
    }
}
