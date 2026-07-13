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

use super::collect::{Collector, PathInfo};

mod handler;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub struct Chain<'a> {
    handlers: Vec<HandlerEntry>,
    analysis: &'a AnalysisPlan,
    paths: &'a Paths,
    collector: &'a Collector,
    hasher: &'a mut StoneDigestWriterHasher,
    pub buckets: BTreeMap<String, Bucket>,
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

    pub fn process(&mut self, paths: impl IntoIterator<Item = PathInfo>) -> Result<(), BoxError> {
        println!("│Analyzing artefacts (» = Include, × = Ignore, ^ = Replace)");

        let mut queue = paths.into_iter().collect::<VecDeque<_>>();

        let pb = ProgressBar::new(queue.len() as u64)
            .with_message("Analyzing")
            .with_style(
                ProgressStyle::with_template("\n|{bar:20.red/blue}| {pos}/{len} {wide_msg}")
                    .unwrap()
                    .progress_chars("■≡=- "),
            );
        pb.tick();

        'paths: while let Some(mut path) = queue.pop_front() {
            pb.set_message(format!("Analyzing {}", path.target_path.display()));

            'handlers: for entry in &self.handlers {
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

                response.generated_paths.into_iter().try_for_each(|path| {
                    let info = self.collector.path(&path, self.hasher)?;

                    queue.push_back(info);

                    Ok(()) as Result<(), BoxError>
                })?;

                match response.decision {
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
                        self.buckets.entry(path.package.clone()).or_default().paths.push(path);
                        continue 'paths;
                    }
                    Decision::ReplaceFile { newpath } => {
                        let newpathinfo = self.collector.path(&newpath, self.hasher)?;
                        pb.println(format!(
                            "│A{} {} » {}",
                            "│ ^".dark_magenta(),
                            format!("{}", path.target_path.display()).dim(),
                            newpathinfo.target_path.display()
                        ));
                        pb.inc(1);
                        self.buckets
                            .entry(newpathinfo.package.clone())
                            .or_default()
                            .paths
                            .push(newpathinfo);
                        continue 'paths;
                    }
                }
            }
        }

        pb.finish_and_clear();
        println!();

        Ok(())
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
        collector.add_rule(super::super::collect::Rule {
            pattern: "*".to_owned(),
            package: "root-output".to_owned(),
            kind: PathRuleKind::Any,
        });
        collector.add_rule(super::super::collect::Rule {
            pattern: "/replacement".to_owned(),
            package: "replacement-output".to_owned(),
            kind: PathRuleKind::Any,
        });

        let mut hasher = StoneDigestWriterHasher::new();
        let original = collector.path(&original, &mut hasher).unwrap();
        assert_eq!(original.package, "root-output");
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
