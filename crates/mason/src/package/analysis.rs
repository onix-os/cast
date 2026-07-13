// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeMap;
use std::{
    collections::{BTreeSet, VecDeque},
    path::PathBuf,
};

use stone::{
    StoneDigestWriterHasher,
    relation::{Dependency, Provider},
};
use stone_recipe::build_policy::AnalyzerKind;
use stone_recipe::derivation::AnalysisPlan;
use tui::{ProgressBar, ProgressStyle, Styled};

use crate::Paths;

use super::collect::{Collector, Error as CollectError, PathInfo, SealedTree};

mod handler;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub struct Chain<'a> {
    handlers: Vec<HandlerEntry>,
    analysis: &'a AnalysisPlan,
    paths: &'a Paths,
    collector: &'a Collector,
    hasher: &'a mut StoneDigestWriterHasher,
    pub buckets: BTreeMap<std::sync::Arc<str>, Bucket>,
}

impl<'a> Chain<'a> {
    pub fn new(
        paths: &'a Paths,
        analysis: &'a AnalysisPlan,
        collector: &'a Collector,
        hasher: &'a mut StoneDigestWriterHasher,
    ) -> Self {
        Self {
            handlers: analysis.handlers.iter().copied().map(HandlerEntry::new).collect(),
            paths,
            analysis,
            collector,
            hasher,
            buckets: Default::default(),
        }
    }

    pub fn process(&mut self, paths: impl IntoIterator<Item = PathInfo>) -> Result<SealedTree, BoxError> {
        let result = self.process_inner(paths);
        if result.is_err() {
            self.collector.poison_inventory();
        }
        result
    }

    fn process_inner(&mut self, paths: impl IntoIterator<Item = PathInfo>) -> Result<SealedTree, BoxError> {
        println!("│Analyzing artefacts (» = Include, × = Ignore, ^ = Replace)");

        let mut queue = VecDeque::new();
        for path in paths {
            queue.try_reserve(1)?;
            queue.push_back(path);
        }

        let pb = ProgressBar::new(queue.len() as u64)
            .with_message("Analyzing")
            .with_style(
                ProgressStyle::with_template("\n|{bar:20.red/blue}| {pos}/{len} {wide_msg}")
                    .unwrap()
                    .progress_chars("■≡=- "),
            );
        pb.tick();

        'paths: while let Some(mut path) = queue.pop_front() {
            path.check_deadline()?;
            pb.set_message(format!("Analyzing {}", path.target_path.display()));

            'handlers: for entry in &self.handlers {
                path.check_deadline()?;
                let response = {
                    let bucket = self.buckets.entry(path.package.clone()).or_default();
                    // Only give handlers ability to update certain bucket
                    // fields. End this borrow before routing generated or
                    // replacement paths through the collector again.
                    let mut bucket_mut = BucketMut {
                        providers: &mut bucket.providers,
                        dependencies: &mut bucket.dependencies,
                        hasher: self.hasher,
                        analysis: self.analysis,
                        paths: self.paths,
                    };

                    entry.handler.handle(&mut bucket_mut, &mut path)?
                };
                path.check_deadline()?;

                let Response {
                    decision,
                    mut generated_paths,
                } = response;

                // Every fallible allocation whose result is needed after a
                // generated batch is admitted must happen before admission.
                // The outer process wrapper poisons the inventory for any
                // remaining post-admission failure.
                if matches!(&decision, Decision::IncludeFile) {
                    self.buckets
                        .entry(path.package.clone())
                        .or_default()
                        .paths
                        .try_reserve(1)?;
                }
                let mut replacement = None;
                let mut generated_replacement = false;
                if let Decision::ReplaceFile { newpath } = &decision {
                    match self.collector.path(newpath, self.hasher) {
                        Ok(info) => {
                            self.buckets
                                .entry(info.package.clone())
                                .or_default()
                                .paths
                                .try_reserve(1)?;
                            replacement = Some(info);
                        }
                        Err(CollectError::UnwitnessedPath { .. }) => {
                            generated_paths.try_reserve(1)?;
                            generated_paths.push(newpath.clone());
                            generated_replacement = true;
                        }
                        Err(error) => return Err(Box::new(error)),
                    }
                }
                queue.try_reserve(generated_paths.len())?;
                let mut generated = self.collector.paths(&generated_paths, self.hasher)?;
                if generated_replacement {
                    replacement = generated.pop();
                    let info = replacement.as_ref().ok_or_else(|| {
                        Box::new(std::io::Error::other("generated replacement was not authenticated")) as BoxError
                    })?;
                    self.buckets
                        .entry(info.package.clone())
                        .or_default()
                        .paths
                        .try_reserve(1)?;
                }
                queue.extend(generated);

                match decision {
                    Decision::NextHandler => continue 'handlers,
                    Decision::IgnoreFile { reason } => {
                        pb.suspend(|| {
                            println!(
                                "│A{} {} {}",
                                "│ ×".yellow(),
                                format!("{}", path.target_path.display()).dim(),
                                format!("({reason})").yellow()
                            );
                        });
                        pb.inc(1);
                        continue 'paths;
                    }
                    Decision::IncludeFile => {
                        pb.suspend(|| println!("│A{} {}", "│ »".green(), path.target_path.display()));
                        pb.inc(1);
                        let paths = &mut self.buckets.entry(path.package.clone()).or_default().paths;
                        paths.push(path);
                        continue 'paths;
                    }
                    Decision::ReplaceFile { .. } => {
                        let newpathinfo = replacement.ok_or_else(|| {
                            Box::new(std::io::Error::other("generated replacement was not authenticated")) as BoxError
                        })?;
                        pb.println(format!(
                            "│A{} {} » {}",
                            "│ ^".dark_magenta(),
                            format!("{}", path.target_path.display()).dim(),
                            newpathinfo.target_path.display()
                        ));
                        pb.inc(1);
                        let paths = &mut self.buckets.entry(newpathinfo.package.clone()).or_default().paths;
                        paths.push(newpathinfo);
                        continue 'paths;
                    }
                }
            }
        }

        pb.finish_and_clear();
        println!();

        self.collector.seal().map_err(Into::into)
    }
}

struct HandlerEntry {
    #[cfg(test)]
    kind: AnalyzerKind,
    handler: Box<dyn Handler>,
}

impl HandlerEntry {
    fn new(kind: AnalyzerKind) -> Self {
        let handler: Box<dyn Handler> = match kind {
            AnalyzerKind::IgnoreBlocked => Box::new(handler::ignore_blocked),
            AnalyzerKind::Binary => Box::new(handler::binary),
            AnalyzerKind::Elf => Box::new(handler::elf),
            AnalyzerKind::PkgConfig => Box::new(handler::pkg_config),
            AnalyzerKind::Python => Box::new(handler::python),
            AnalyzerKind::CMake => Box::new(handler::cmake),
            AnalyzerKind::CompressMan => Box::new(handler::compressman),
            AnalyzerKind::IncludeAny => Box::new(handler::include_any),
        };
        Self {
            #[cfg(test)]
            kind,
            handler,
        }
    }
}

#[derive(Debug, Default)]
pub struct Bucket {
    providers: BTreeSet<Provider>,
    dependencies: BTreeSet<Dependency>,
    pub paths: Vec<PathInfo>,
}

impl Bucket {
    pub fn providers(&self) -> impl Iterator<Item = &Provider> {
        self.providers.iter()
    }

    pub fn dependencies(&self) -> impl Iterator<Item = &Dependency> {
        // We shouldn't self depend on things we provide
        self.dependencies
            .iter()
            .filter(|d| !self.providers.iter().any(|p| p.kind == d.kind && p.name == d.name))
    }
}

pub struct BucketMut<'a> {
    pub providers: &'a mut BTreeSet<Provider>,
    pub dependencies: &'a mut BTreeSet<Dependency>,
    pub hasher: &'a mut StoneDigestWriterHasher,
    pub analysis: &'a AnalysisPlan,
    pub paths: &'a Paths,
}

pub struct Response {
    pub decision: Decision,
    pub generated_paths: Vec<PathBuf>,
}

pub enum Decision {
    NextHandler,
    IgnoreFile { reason: String },
    IncludeFile,
    ReplaceFile { newpath: PathBuf },
}

impl From<Decision> for Response {
    fn from(decision: Decision) -> Self {
        Self {
            decision,
            generated_paths: vec![],
        }
    }
}

pub trait Handler {
    fn handle(&self, bucket: &mut BucketMut<'_>, path: &mut PathInfo) -> Result<Response, BoxError>;
}

impl<T> Handler for T
where
    T: Fn(&mut BucketMut<'_>, &mut PathInfo) -> Result<Response, BoxError>,
{
    fn handle(&self, bucket: &mut BucketMut<'_>, path: &mut PathInfo) -> Result<Response, BoxError> {
        (self)(bucket, path)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use fs_err as fs;
    use stone_recipe::derivation::PathRuleKind;

    use super::*;
    use crate::{Recipe, package::test_derivation_plan};

    struct ReplaceWith(PathBuf);

    impl Handler for ReplaceWith {
        fn handle(&self, _bucket: &mut BucketMut<'_>, _path: &mut PathInfo) -> Result<Response, BoxError> {
            Ok(Decision::ReplaceFile {
                newpath: self.0.clone(),
            }
            .into())
        }
    }

    struct GenerateThenFail {
        original: PathBuf,
        generated: PathBuf,
    }

    impl Handler for GenerateThenFail {
        fn handle(&self, _bucket: &mut BucketMut<'_>, path: &mut PathInfo) -> Result<Response, BoxError> {
            if path.path == self.original {
                fs::write(&self.generated, b"generated")?;
                return Ok(Response {
                    decision: Decision::IncludeFile,
                    generated_paths: vec![self.generated.clone()],
                });
            }

            Err(Box::new(std::io::Error::other(
                "deterministic failure after generated admission",
            )))
        }
    }

    #[test]
    fn replacement_is_routed_to_the_output_selected_for_the_new_path() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();

        let install = tempfile::tempdir().unwrap();
        let original = install.path().join("original");
        let replacement = install.path().join("replacement");
        fs::write(&original, b"original").unwrap();
        fs::write(&replacement, b"replacement").unwrap();
        let mut collector = Collector::new(install.path());
        collector.add_rule("*", "root-output", PathRuleKind::Any).unwrap();
        collector
            .add_rule("/replacement", "replacement-output", PathRuleKind::Any)
            .unwrap();

        let mut hasher = StoneDigestWriterHasher::new();
        let original = collector.path(&original, &mut hasher).unwrap();
        assert_eq!(original.package.as_ref(), "root-output");
        let mut chain = Chain::new(&paths, &plan.analysis, &collector, &mut hasher);
        chain.handlers = vec![HandlerEntry {
            kind: AnalyzerKind::IncludeAny,
            handler: Box::new(ReplaceWith(replacement)),
        }];

        chain.process([original]).unwrap();

        assert!(chain.buckets["root-output"].paths.is_empty());
        assert_eq!(chain.buckets["replacement-output"].paths.len(), 1);
        assert_eq!(
            chain.buckets["replacement-output"].paths[0].target_path,
            Path::new("/replacement")
        );
    }

    #[test]
    fn any_failure_after_generated_admission_poisons_the_inventory() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let plan = test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();

        let install = tempfile::tempdir().unwrap();
        let original_path = install.path().join("original");
        let generated = install.path().join("generated");
        fs::write(&original_path, b"original").unwrap();
        let mut collector = Collector::new(install.path());
        collector.add_rule("*", "out", PathRuleKind::Any).unwrap();

        let mut hasher = StoneDigestWriterHasher::new();
        let original = collector.path(&original_path, &mut hasher).unwrap();
        let mut chain = Chain::new(&paths, &plan.analysis, &collector, &mut hasher);
        chain.handlers = vec![HandlerEntry {
            kind: AnalyzerKind::IncludeAny,
            handler: Box::new(GenerateThenFail {
                original: original_path,
                generated,
            }),
        }];

        assert!(chain.process([original]).is_err());
        drop(chain);
        assert!(matches!(collector.seal(), Err(CollectError::InventoryPoisoned)));
    }

    #[test]
    fn chain_uses_only_the_declared_handlers_in_exact_order() {
        let recipe =
            Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut plan = test_derivation_plan();
        let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
        let install = tempfile::tempdir().unwrap();
        let collector = Collector::new(install.path());
        let mut hasher = StoneDigestWriterHasher::new();

        let all = vec![
            AnalyzerKind::IgnoreBlocked,
            AnalyzerKind::Binary,
            AnalyzerKind::Elf,
            AnalyzerKind::PkgConfig,
            AnalyzerKind::Python,
            AnalyzerKind::CMake,
            AnalyzerKind::CompressMan,
            AnalyzerKind::IncludeAny,
        ];
        plan.analysis.handlers = all.clone();
        let chain = Chain::new(&paths, &plan.analysis, &collector, &mut hasher);
        assert_eq!(chain.handlers.iter().map(|entry| entry.kind).collect::<Vec<_>>(), all);
        assert_eq!(chain.handlers.len(), plan.analysis.handlers.len());
        drop(chain);

        let first = vec![
            AnalyzerKind::IgnoreBlocked,
            AnalyzerKind::CMake,
            AnalyzerKind::IncludeAny,
        ];
        plan.analysis.handlers = first.clone();
        let chain = Chain::new(&paths, &plan.analysis, &collector, &mut hasher);
        assert_eq!(chain.handlers.iter().map(|entry| entry.kind).collect::<Vec<_>>(), first);
        assert_eq!(chain.handlers.len(), plan.analysis.handlers.len());
        drop(chain);

        let second = vec![
            AnalyzerKind::CMake,
            AnalyzerKind::IgnoreBlocked,
            AnalyzerKind::IncludeAny,
        ];
        plan.analysis.handlers = second.clone();
        let chain = Chain::new(&paths, &plan.analysis, &collector, &mut hasher);
        assert_eq!(
            chain.handlers.iter().map(|entry| entry.kind).collect::<Vec<_>>(),
            second
        );
        assert_eq!(chain.handlers.len(), plan.analysis.handlers.len());
        assert_ne!(first, second);
    }
}
