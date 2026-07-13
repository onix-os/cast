// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Stable, comprehensive presentation of a frozen derivation and its
//! evaluation provenance.

use std::fmt::{Arguments, Write as _};

use stone_recipe::derivation::{
    AnalysisToolchain, DerivationPlan, LockedIdentity, LockedSource, NetworkMode, OutputRelation, PathRuleKind,
    Platform, RelationKind, RelationPlan, StepPlan,
};

use stone_recipe::build_policy::layers::BuildPolicyOperation;

pub(super) fn format(plan: &DerivationPlan) -> String {
    Explanation { plan }.render()
}

struct Explanation<'a> {
    plan: &'a DerivationPlan,
}

impl Explanation<'_> {
    fn render(&self) -> String {
        let mut formatter = Formatter::default();
        formatter.open(0, "derivation_explain");
        self.schema(&mut formatter);
        self.identity(&mut formatter);
        self.recipe_provenance(&mut formatter);
        self.package(&mut formatter);
        self.platforms(&mut formatter);
        self.resolved_identities(&mut formatter);
        self.profile_fragments(&mut formatter);
        self.policy_provenance(&mut formatter);
        self.locked_sources(&mut formatter);
        self.requested_providers(&mut formatter);
        self.repositories(&mut formatter);
        self.lock_requests(&mut formatter);
        self.locked_packages(&mut formatter);
        self.jobs(&mut formatter);
        self.environment(&mut formatter);
        self.layout(&mut formatter);
        self.execution(&mut formatter);
        self.analyzers(&mut formatter);
        self.analysis(&mut formatter);
        self.manifest_build_inputs(&mut formatter);
        self.collection_rules(&mut formatter);
        self.outputs(&mut formatter);
        self.result(&mut formatter);
        formatter.close(0);
        formatter.finish()
    }

    fn schema(&self, formatter: &mut Formatter) {
        formatter.open(1, "schema");
        formatter.field(2, "derivation_plan", self.plan.schema_version);
        formatter.field(2, "build_lock", self.plan.build_lock.schema_version);
        formatter.close(1);
    }

    fn identity(&self, formatter: &mut Formatter) {
        formatter.open(1, "identity");
        formatter.string(2, "boulder_version", &self.plan.boulder_version);
        formatter.string(2, "boulder_fingerprint", &self.plan.boulder_fingerprint);
        formatter.string(2, "recipe", &self.plan.provenance.recipe.sha256);
        formatter.string(2, "source_lock", &self.plan.source_lock_digest);
        formatter.string(2, "build_lock", &self.plan.build_lock.digest());
        formatter.string(2, "request", &self.plan.build_lock.request_fingerprint);
        formatter.close(1);
    }

    fn recipe_provenance(&self, formatter: &mut Formatter) {
        formatter.open(1, "recipe_provenance");
        format_evaluation_fingerprint(formatter, 2, &self.plan.provenance.recipe);
        formatter.close(1);
    }

    fn package(&self, formatter: &mut Formatter) {
        let package = &self.plan.package;
        let mut licenses = package.licenses.iter().map(String::as_str).collect::<Vec<_>>();
        licenses.sort_unstable();

        formatter.open(1, "package");
        formatter.string(2, "name", &package.name);
        formatter.string(2, "version", &package.version);
        formatter.field(2, "source_release", package.source_release);
        formatter.field(2, "build_release", package.build_release);
        formatter.string(2, "homepage", &package.homepage);
        formatter.string_list(2, "licenses", licenses);
        formatter.string(2, "architecture", &package.architecture);
        formatter.close(1);
    }

    fn platforms(&self, formatter: &mut Formatter) {
        formatter.open(1, "platforms");
        format_platform(formatter, 2, "build", &self.plan.build_lock.build_platform);
        format_platform(formatter, 2, "host", &self.plan.build_lock.host_platform);
        format_platform(formatter, 2, "target", &self.plan.build_lock.target_platform);
        formatter.close(1);
    }

    fn resolved_identities(&self, formatter: &mut Formatter) {
        formatter.open(1, "resolved_identities");
        format_locked_identity(formatter, 2, "policy", &self.plan.build_lock.policy);
        format_locked_identity(formatter, 2, "target", &self.plan.build_lock.target);
        format_locked_identity(formatter, 2, "profile", &self.plan.build_lock.profile);
        format_locked_identity(formatter, 2, "toolchain", &self.plan.build_lock.toolchain);
        format_locked_identity(formatter, 2, "builder", &self.plan.build_lock.builder);
        formatter.close(1);
    }

    fn profile_fragments(&self, formatter: &mut Formatter) {
        formatter.open(1, "profile_fragments");
        for (order, fragment) in self.plan.provenance.profiles.iter().enumerate() {
            formatter.indexed_open(2, "fragment", order);
            formatter.field(3, "order", order);
            formatter.string(3, "logical_name", &fragment.logical_name);
            format_evaluation_fingerprint(formatter, 3, &fragment.evaluation);
            formatter.close(2);
        }
        formatter.close(1);
    }

    fn policy_provenance(&self, formatter: &mut Formatter) {
        let policy = &self.plan.provenance.policy;
        let mut global_order = 0;

        formatter.open(1, "policy_provenance");
        formatter.string(2, "name", &policy.name);
        formatter.open(2, "final_root");
        format_evaluation_fingerprint(formatter, 3, &policy.root);
        formatter.close(2);
        formatter.open(2, "layers");
        for (layer_order, layer) in policy.layers.iter().enumerate() {
            formatter.indexed_open(3, "layer", layer_order);
            formatter.field(4, "layer_order", layer_order);
            formatter.string(4, "name", &layer.name);
            formatter.open(4, "transitions");
            for (entry_order, transition) in layer.transitions.iter().enumerate() {
                formatter.indexed_open(5, "transition", entry_order);
                formatter.field(6, "layer_order", layer_order);
                formatter.field(6, "entry_order", entry_order);
                formatter.field(6, "global_order", global_order);
                formatter.string(6, "operation", policy_operation(transition.operation));
                formatter.string(6, "origin", &transition.origin);
                format_evaluation_fingerprint(formatter, 6, &transition.evaluation);
                formatter.close(5);
                global_order += 1;
            }
            formatter.close(4);
            formatter.close(3);
        }
        formatter.close(2);
        formatter.close(1);
    }

    fn locked_sources(&self, formatter: &mut Formatter) {
        let mut sources = self.plan.sources.iter().collect::<Vec<_>>();
        sources.sort_by_key(|source| source.order());

        formatter.open(1, "locked_sources");
        for source in sources {
            formatter.indexed_open(2, "source", source.order());
            match source {
                LockedSource::Archive {
                    order,
                    url,
                    sha256,
                    filename,
                } => {
                    formatter.string(3, "kind", "archive");
                    formatter.field(3, "order", order);
                    formatter.string(3, "url", url);
                    formatter.string(3, "sha256", sha256);
                    formatter.string(3, "filename", filename);
                }
                LockedSource::Git {
                    order,
                    url,
                    requested_ref,
                    commit,
                    directory,
                } => {
                    formatter.string(3, "kind", "git");
                    formatter.field(3, "order", order);
                    formatter.string(3, "url", url);
                    formatter.string(3, "requested_ref", requested_ref);
                    formatter.string(3, "commit", commit);
                    formatter.string(3, "directory", directory);
                }
            }
            formatter.close(2);
        }
        formatter.close(1);
    }

    fn requested_providers(&self, formatter: &mut Formatter) {
        let mut providers = self
            .plan
            .build_lock
            .requests
            .iter()
            .map(|request| request.request.as_str())
            .collect::<Vec<_>>();
        providers.sort_unstable();

        formatter.open(1, "requested_providers");
        for (index, provider) in providers.into_iter().enumerate() {
            formatter.indexed_open(2, "provider", index);
            formatter.string(3, "request", provider);
            formatter.close(2);
        }
        formatter.close(1);
    }

    fn repositories(&self, formatter: &mut Formatter) {
        let mut repositories = self.plan.build_lock.repositories.iter().collect::<Vec<_>>();
        repositories.sort_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then_with(|| left.snapshot.cmp(&right.snapshot))
                .then_with(|| left.index_uri.cmp(&right.index_uri))
        });

        formatter.open(1, "repositories");
        for (index, repository) in repositories.into_iter().enumerate() {
            formatter.indexed_open(2, "repository", index);
            formatter.string(3, "id", &repository.id);
            formatter.string(3, "index_uri", &repository.index_uri);
            formatter.string(3, "snapshot", &repository.snapshot);
            formatter.close(2);
        }
        formatter.close(1);
    }

    fn lock_requests(&self, formatter: &mut Formatter) {
        let mut requests = self.plan.build_lock.requests.iter().collect::<Vec<_>>();
        requests.sort_by(|left, right| {
            left.request
                .cmp(&right.request)
                .then_with(|| left.package_id.cmp(&right.package_id))
                .then_with(|| left.output.cmp(&right.output))
        });

        formatter.open(1, "lock_requests");
        for (index, request) in requests.into_iter().enumerate() {
            formatter.indexed_open(2, "request", index);
            formatter.string(3, "provider", &request.request);
            formatter.string(3, "package_id", &request.package_id);
            formatter.string(3, "output", &request.output);
            formatter.close(2);
        }
        formatter.close(1);
    }

    fn locked_packages(&self, formatter: &mut Formatter) {
        let mut packages = self.plan.build_lock.packages.iter().collect::<Vec<_>>();
        packages.sort_by(|left, right| left.package_id.cmp(&right.package_id));

        formatter.open(1, "locked_packages");
        for (index, package) in packages.into_iter().enumerate() {
            formatter.indexed_open(2, "package", index);
            formatter.string(3, "package_id", &package.package_id);
            formatter.string(3, "name", &package.name);
            formatter.string(3, "version", &package.version);
            formatter.string(3, "architecture", &package.architecture);
            formatter.string(3, "repository", &package.repository);

            let mut outputs = package.outputs.iter().collect::<Vec<_>>();
            outputs.sort_by(|left, right| left.name.cmp(&right.name));
            formatter.open(3, "outputs");
            for (output_index, output) in outputs.into_iter().enumerate() {
                formatter.indexed_open(4, "output", output_index);
                formatter.string(5, "name", &output.name);
                formatter.close(4);
            }
            formatter.close(3);

            let mut dependencies = package.dependencies.iter().collect::<Vec<_>>();
            dependencies.sort();
            formatter.open(3, "dependencies");
            for (dependency_index, dependency) in dependencies.into_iter().enumerate() {
                formatter.indexed_open(4, "dependency", dependency_index);
                formatter.string(5, "package_id", &dependency.package_id);
                formatter.string(5, "output", &dependency.output);
                formatter.close(4);
            }
            formatter.close(3);
            formatter.close(2);
        }
        formatter.close(1);
    }

    fn jobs(&self, formatter: &mut Formatter) {
        formatter.open(1, "jobs");
        for (job_index, job) in self.plan.jobs.iter().enumerate() {
            formatter.indexed_open(2, "job", job_index);
            formatter.optional_string(3, "pgo_stage", job.pgo_stage.as_deref());
            formatter.optional_string(3, "pgo_dir", job.pgo_dir.as_deref());
            formatter.string(3, "build_dir", &job.build_dir);
            formatter.string(3, "work_dir", &job.work_dir);
            formatter.open(3, "phases");
            for (phase_index, phase) in job.phases.iter().enumerate() {
                formatter.indexed_open(4, "phase", phase_index);
                formatter.string(5, "name", &phase.name);
                format_steps(formatter, 5, "pre", &phase.pre);
                format_steps(formatter, 5, "steps", &phase.steps);
                format_steps(formatter, 5, "post", &phase.post);
                formatter.close(4);
            }
            formatter.close(3);
            formatter.close(2);
        }
        formatter.close(1);
    }

    fn environment(&self, formatter: &mut Formatter) {
        formatter.open(1, "environment");
        for (name, value) in &self.plan.environment {
            formatter.map_entry(2, name, value);
        }
        formatter.close(1);
    }

    fn layout(&self, formatter: &mut Formatter) {
        formatter.open(1, "layout");
        formatter.string(2, "hostname", &self.plan.layout.hostname);
        formatter.string(2, "guest_root", &self.plan.layout.guest_root);
        formatter.string(2, "artifacts_dir", &self.plan.layout.artifacts_dir);
        formatter.string(2, "build_dir", &self.plan.layout.build_dir);
        formatter.string(2, "source_dir", &self.plan.layout.source_dir);
        formatter.string(2, "recipe_dir", &self.plan.layout.recipe_dir);
        formatter.string(2, "install_dir", &self.plan.layout.install_dir);
        formatter.string(2, "package_dir", &self.plan.layout.package_dir);
        formatter.string(2, "ccache_dir", &self.plan.layout.ccache_dir);
        formatter.string(2, "sccache_dir", &self.plan.layout.sccache_dir);
        formatter.string(2, "go_cache_dir", &self.plan.layout.go_cache_dir);
        formatter.string(2, "go_mod_cache_dir", &self.plan.layout.go_mod_cache_dir);
        formatter.string(2, "cargo_cache_dir", &self.plan.layout.cargo_cache_dir);
        formatter.string(2, "zig_cache_dir", &self.plan.layout.zig_cache_dir);
        formatter.close(1);
    }

    fn execution(&self, formatter: &mut Formatter) {
        formatter.open(1, "execution");
        formatter.string(
            2,
            "network",
            match self.plan.execution.network {
                NetworkMode::Disabled => "disabled",
                NetworkMode::Enabled => "enabled",
            },
        );
        formatter.open(2, "filesystems");
        formatter.string(3, "proc", self.plan.execution.filesystems.proc.as_str());
        formatter.string(3, "tmp", self.plan.execution.filesystems.tmp.as_str());
        formatter.string(3, "sys", self.plan.execution.filesystems.sys.as_str());
        formatter.string(3, "dev", self.plan.execution.filesystems.dev.as_str());
        formatter.close(2);
        formatter.field(2, "compiler_cache", self.plan.execution.compiler_cache);
        formatter.field(2, "jobs", self.plan.execution.jobs);
        formatter.close(1);
    }

    fn analyzers(&self, formatter: &mut Formatter) {
        formatter.open(1, "analyzers");
        for (order, handler) in self.plan.analysis.handlers.iter().enumerate() {
            formatter.indexed_open(2, "analyzer", order);
            formatter.field(3, "order", order);
            formatter.string(3, "kind", handler.as_str());
            formatter.close(2);
        }
        formatter.close(1);
    }

    fn analysis(&self, formatter: &mut Formatter) {
        formatter.open(1, "analysis");
        formatter.string(
            2,
            "toolchain",
            match self.plan.analysis.toolchain {
                AnalysisToolchain::Llvm => "llvm",
                AnalysisToolchain::Gnu => "gnu",
            },
        );
        formatter.field(2, "debug", self.plan.analysis.debug);
        formatter.field(2, "strip", self.plan.analysis.strip);
        formatter.field(2, "compress_man", self.plan.analysis.compress_man);
        formatter.field(2, "remove_libtool", self.plan.analysis.remove_libtool);
        formatter.close(1);
    }

    fn manifest_build_inputs(&self, formatter: &mut Formatter) {
        let mut inputs = self.plan.manifest_build_inputs.iter().collect::<Vec<_>>();
        inputs.sort();

        formatter.open(1, "manifest_build_inputs");
        for (index, relation) in inputs.into_iter().enumerate() {
            format_relation(formatter, 2, "input", index, relation);
        }
        formatter.close(1);
    }

    fn collection_rules(&self, formatter: &mut Formatter) {
        formatter.open(1, "collection_rules");
        for (order, rule) in self.plan.collection_rules.iter().enumerate() {
            formatter.indexed_open(2, "rule", order);
            formatter.field(3, "order", order);
            formatter.string(3, "output", &rule.output);
            formatter.string(3, "kind", path_rule_kind(rule.kind));
            formatter.string(3, "pattern", &rule.pattern);
            formatter.close(2);
        }
        formatter.close(1);
    }

    fn outputs(&self, formatter: &mut Formatter) {
        let mut outputs = self.plan.outputs.iter().collect::<Vec<_>>();
        outputs.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.package_name.cmp(&right.package_name))
        });

        formatter.open(1, "outputs");
        for (index, output) in outputs.into_iter().enumerate() {
            formatter.indexed_open(2, "output", index);
            formatter.string(3, "name", &output.name);
            formatter.string(3, "package_name", &output.package_name);
            formatter.field(3, "include_in_manifest", output.include_in_manifest);
            formatter.optional_string(3, "summary", output.summary.as_deref());
            formatter.optional_string(3, "description", output.description.as_deref());

            let mut provides_exclude = output.provides_exclude.iter().map(String::as_str).collect::<Vec<_>>();
            provides_exclude.sort_unstable();
            formatter.string_list(3, "provides_exclude", provides_exclude);
            let mut runtime_exclude = output.runtime_exclude.iter().map(String::as_str).collect::<Vec<_>>();
            runtime_exclude.sort_unstable();
            formatter.string_list(3, "runtime_exclude", runtime_exclude);

            let mut runtime_inputs = output.runtime_inputs.iter().collect::<Vec<_>>();
            runtime_inputs.sort();
            formatter.open(3, "runtime_inputs");
            for (relation_index, relation) in runtime_inputs.into_iter().enumerate() {
                formatter.indexed_open(4, "relation", relation_index);
                match relation {
                    OutputRelation::Locked { relation, reference } => {
                        formatter.string(5, "kind", "locked");
                        formatter.string(5, "relation_kind", relation_kind(relation.kind));
                        formatter.string(5, "name", &relation.name);
                        formatter.string(5, "package_id", &reference.package_id);
                        formatter.string(5, "output", &reference.output);
                    }
                    OutputRelation::Planned { output } => {
                        formatter.string(5, "kind", "planned");
                        formatter.string(5, "output", output);
                    }
                }
                formatter.close(4);
            }
            formatter.close(3);

            let mut conflicts = output.conflicts.iter().collect::<Vec<_>>();
            conflicts.sort();
            formatter.open(3, "conflicts");
            for (conflict_index, conflict) in conflicts.into_iter().enumerate() {
                format_relation(formatter, 4, "conflict", conflict_index, conflict);
            }
            formatter.close(3);
            formatter.close(2);
        }
        formatter.close(1);
    }

    fn result(&self, formatter: &mut Formatter) {
        formatter.open(1, "result");
        formatter.field(2, "source_date_epoch", self.plan.source_date_epoch);
        formatter.string(2, "derivation_id", self.plan.derivation_id().as_str());
        formatter.close(1);
    }
}

fn format_platform(formatter: &mut Formatter, indent: usize, name: &str, platform: &Platform) {
    formatter.open(indent, name);
    formatter.string(indent + 1, "architecture", &platform.architecture);
    formatter.string(indent + 1, "vendor", &platform.vendor);
    formatter.string(indent + 1, "operating_system", &platform.operating_system);
    formatter.string(indent + 1, "abi", &platform.abi);
    formatter.close(indent);
}

fn format_locked_identity(formatter: &mut Formatter, indent: usize, name: &str, identity: &LockedIdentity) {
    formatter.open(indent, name);
    formatter.string(indent + 1, "name", &identity.name);
    formatter.string(indent + 1, "fingerprint", &identity.fingerprint);
    formatter.close(indent);
}

fn format_evaluation_fingerprint(
    formatter: &mut Formatter,
    indent: usize,
    fingerprint: &gluon_config::EvaluationFingerprint,
) {
    formatter.open(indent, "evaluation");
    formatter.string(indent + 1, "root_logical_name", &fingerprint.root_logical_name);
    formatter.string(indent + 1, "root_source_sha256", &fingerprint.root_source_sha256);
    formatter.string(indent + 1, "gluon_version", fingerprint.gluon_version);
    formatter.field(
        indent + 1,
        "configuration_abi_version",
        fingerprint.configuration_abi_version,
    );
    formatter.field(
        indent + 1,
        "evaluator_policy_version",
        fingerprint.evaluator_policy_version,
    );
    formatter.string(
        indent + 1,
        "explicit_inputs_sha256",
        &fingerprint.explicit_inputs_sha256,
    );
    formatter.string(indent + 1, "sha256", &fingerprint.sha256);

    formatter.open(indent + 1, "imported_modules");
    for (index, module) in fingerprint.imported_modules.iter().enumerate() {
        formatter.indexed_open(indent + 2, "module", index);
        formatter.string(indent + 3, "logical_name", &module.logical_name);
        formatter.string(indent + 3, "sha256", &module.sha256);
        formatter.close(indent + 2);
    }
    formatter.close(indent + 1);
    formatter.close(indent);
}

const fn policy_operation(operation: BuildPolicyOperation) -> &'static str {
    match operation {
        BuildPolicyOperation::Add => "add",
        BuildPolicyOperation::Replace => "replace",
        BuildPolicyOperation::Modify => "modify",
    }
}

fn format_steps(formatter: &mut Formatter, indent: usize, name: &str, steps: &[StepPlan]) {
    formatter.open(indent, name);
    for (index, step) in steps.iter().enumerate() {
        formatter.indexed_open(indent + 1, "step", index);
        match step {
            StepPlan::Run {
                program,
                args,
                environment,
                working_dir,
            } => {
                formatter.string(indent + 2, "kind", "run");
                formatter.string(indent + 2, "program", program);
                formatter.string_list(indent + 2, "args", args.iter().map(String::as_str));
                formatter.open(indent + 2, "environment");
                for (key, value) in environment {
                    formatter.map_entry(indent + 3, key, value);
                }
                formatter.close(indent + 2);
                formatter.string(indent + 2, "working_dir", working_dir);
            }
            StepPlan::Shell {
                interpreter,
                script,
                environment,
                working_dir,
            } => {
                formatter.string(indent + 2, "kind", "shell");
                formatter.string(indent + 2, "interpreter", interpreter);
                formatter.string(indent + 2, "script", script);
                formatter.open(indent + 2, "environment");
                for (key, value) in environment {
                    formatter.map_entry(indent + 3, key, value);
                }
                formatter.close(indent + 2);
                formatter.string(indent + 2, "working_dir", working_dir);
            }
        }
        formatter.close(indent + 1);
    }
    formatter.close(indent);
}

fn format_relation(formatter: &mut Formatter, indent: usize, label: &str, index: usize, relation: &RelationPlan) {
    formatter.indexed_open(indent, label, index);
    formatter.string(indent + 1, "kind", relation_kind(relation.kind));
    formatter.string(indent + 1, "name", &relation.name);
    formatter.close(indent);
}

const fn relation_kind(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::PackageName => "package_name",
        RelationKind::SharedLibrary => "shared_library",
        RelationKind::PkgConfig => "pkg_config",
        RelationKind::Interpreter => "interpreter",
        RelationKind::CMake => "cmake",
        RelationKind::Python => "python",
        RelationKind::Binary => "binary",
        RelationKind::SystemBinary => "system_binary",
        RelationKind::PkgConfig32 => "pkg_config_32",
    }
}

const fn path_rule_kind(kind: PathRuleKind) -> &'static str {
    match kind {
        PathRuleKind::Any => "any",
        PathRuleKind::Executable => "executable",
        PathRuleKind::Symlink => "symlink",
        PathRuleKind::Special => "special",
    }
}

#[derive(Default)]
struct Formatter {
    output: String,
}

impl Formatter {
    fn finish(self) -> String {
        self.output
    }

    fn open(&mut self, indent: usize, name: &str) {
        self.line(indent, format_args!("{name} {{"));
    }

    fn indexed_open(&mut self, indent: usize, name: &str, index: impl std::fmt::Display) {
        self.line(indent, format_args!("{name} {index} {{"));
    }

    fn close(&mut self, indent: usize) {
        self.line(indent, format_args!("}}"));
    }

    fn field(&mut self, indent: usize, name: &str, value: impl std::fmt::Display) {
        self.line(indent, format_args!("{name} = {value}"));
    }

    fn string(&mut self, indent: usize, name: &str, value: &str) {
        self.line(indent, format_args!("{name} = {value:?}"));
    }

    fn optional_string(&mut self, indent: usize, name: &str, value: Option<&str>) {
        match value {
            Some(value) => self.string(indent, name, value),
            None => self.line(indent, format_args!("{name} = null")),
        }
    }

    fn string_list<'a>(&mut self, indent: usize, name: &str, values: impl IntoIterator<Item = &'a str>) {
        let mut rendered = String::from("[");
        for (index, value) in values.into_iter().enumerate() {
            if index > 0 {
                rendered.push_str(", ");
            }
            write!(rendered, "{value:?}").expect("writing to a String cannot fail");
        }
        rendered.push(']');
        self.line(indent, format_args!("{name} = {rendered}"));
    }

    fn map_entry(&mut self, indent: usize, name: &str, value: &str) {
        self.line(indent, format_args!("{name:?} = {value:?}"));
    }

    fn line(&mut self, indent: usize, value: Arguments<'_>) {
        for _ in 0..indent {
            self.output.push_str("  ");
        }
        self.output.write_fmt(value).expect("writing to a String cannot fail");
        self.output.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gluon_config::{EvaluationFingerprint, Evaluator, ImportPolicy, Source};
    use stone_recipe::{
        build_policy::{AnalyzerKind, layers::BuildPolicyOperation},
        derivation::{
            AnalysisPlan, BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuilderLayout, CollectionRulePlan,
            DERIVATION_PLAN_SCHEMA_VERSION, DerivationProvenance, ExecutionPolicy, JobPlan, LockedOutput,
            LockedOutputRef, LockedPackage, LockedRequest, OutputPlan, PackageIdentity, PhasePlan,
            PolicyLayerProvenance, PolicyProvenance, PolicyTransitionProvenance, ProfileFragmentProvenance,
            RepositorySnapshot, policy_composition_identity, profile_aggregate_fingerprint,
        },
    };

    use super::*;

    struct Fixture {
        plan: DerivationPlan,
    }

    impl Fixture {
        fn render(&self) -> String {
            format(&self.plan)
        }
    }

    fn identity(name: &str) -> LockedIdentity {
        LockedIdentity {
            name: name.to_owned(),
            fingerprint: format!("{name}-fingerprint"),
        }
    }

    fn evaluation(logical_name: &str, source: &str, explicit_inputs: &[u8]) -> EvaluationFingerprint {
        Evaluator::default()
            .evaluate_with_inputs::<i64>(&Source::new(logical_name, source), explicit_inputs)
            .expect("fixture evaluation must succeed")
            .fingerprint
    }

    fn evaluation_with_import(logical_name: &str, explicit_inputs: &[u8]) -> EvaluationFingerprint {
        let policy = ImportPolicy::new()
            .with_embedded_module("fixture.provenance", "41")
            .expect("fixture module name must be valid");
        Evaluator::default()
            .with_import_policy(policy)
            .evaluate_with_inputs::<i64>(
                &Source::new(logical_name, "import! fixture.provenance"),
                explicit_inputs,
            )
            .expect("fixture import evaluation must succeed")
            .fingerprint
    }

    fn fixture() -> Fixture {
        const SOURCE_LOCK_BYTES: &[u8] = b"canonical explanation source lock";

        let profiles = vec![
            ProfileFragmentProvenance {
                logical_name: "vendor/base".to_owned(),
                evaluation: evaluation_with_import("profile.d/base.glu", &[]),
            },
            ProfileFragmentProvenance {
                logical_name: "site/local".to_owned(),
                evaluation: evaluation("profile.d/local.glu", "42", &[]),
            },
        ];
        let layers = vec![
            PolicyLayerProvenance {
                name: "foundation".to_owned(),
                transitions: vec![PolicyTransitionProvenance {
                    operation: BuildPolicyOperation::Add,
                    origin: "default.glu".to_owned(),
                    evaluation: evaluation_with_import("default.glu", &[]),
                }],
            },
            PolicyLayerProvenance {
                name: "site-empty".to_owned(),
                transitions: Vec::new(),
            },
            PolicyLayerProvenance {
                name: "override".to_owned(),
                transitions: vec![PolicyTransitionProvenance {
                    operation: BuildPolicyOperation::Modify,
                    origin: "override.glu".to_owned(),
                    evaluation: evaluation("override.glu", "43", &[]),
                }],
            },
        ];
        let policy_inputs = policy_composition_identity("repository-policy", &layers);
        let provenance = DerivationProvenance {
            recipe: evaluation_with_import("stone.glu", SOURCE_LOCK_BYTES),
            profiles,
            policy: PolicyProvenance {
                name: "repository-policy".to_owned(),
                root: evaluation("policy.glu", "44", &policy_inputs),
                layers,
            },
        };
        let lock = BuildLock {
            schema_version: BUILD_LOCK_SCHEMA_VERSION,
            request_fingerprint: "locked-request-fingerprint".to_owned(),
            repositories: vec![
                RepositorySnapshot {
                    id: "z-repository".to_owned(),
                    index_uri: "https://z.invalid/index".to_owned(),
                    snapshot: "z-snapshot".to_owned(),
                },
                RepositorySnapshot {
                    id: "a-repository".to_owned(),
                    index_uri: "https://a.invalid/index".to_owned(),
                    snapshot: "a-snapshot".to_owned(),
                },
            ],
            requests: vec![
                LockedRequest {
                    request: "pkg(zeta)".to_owned(),
                    package_id: "zeta-id".to_owned(),
                    output: "devel".to_owned(),
                },
                LockedRequest {
                    request: "binary(alpha)".to_owned(),
                    package_id: "alpha-id".to_owned(),
                    output: "out".to_owned(),
                },
            ],
            packages: vec![
                LockedPackage {
                    package_id: "zeta-id".to_owned(),
                    name: "zeta".to_owned(),
                    version: "2.0-1-1".to_owned(),
                    architecture: "x86_64".to_owned(),
                    repository: "z-repository".to_owned(),
                    outputs: vec![
                        LockedOutput { name: "out".to_owned() },
                        LockedOutput {
                            name: "devel".to_owned(),
                        },
                    ],
                    dependencies: vec![LockedOutputRef {
                        package_id: "alpha-id".to_owned(),
                        output: "out".to_owned(),
                    }],
                },
                LockedPackage {
                    package_id: "alpha-id".to_owned(),
                    name: "alpha".to_owned(),
                    version: "1.0-1-1".to_owned(),
                    architecture: "x86_64".to_owned(),
                    repository: "a-repository".to_owned(),
                    outputs: vec![LockedOutput { name: "out".to_owned() }],
                    dependencies: Vec::new(),
                },
            ],
            build_platform: Platform {
                architecture: "x86_64".to_owned(),
                vendor: "unknown".to_owned(),
                operating_system: "linux".to_owned(),
                abi: "gnu".to_owned(),
            },
            host_platform: Platform {
                architecture: "x86_64".to_owned(),
                vendor: "aeryn".to_owned(),
                operating_system: "linux".to_owned(),
                abi: "gnu".to_owned(),
            },
            target_platform: Platform {
                architecture: "x86_64".to_owned(),
                vendor: "aeryn".to_owned(),
                operating_system: "linux".to_owned(),
                abi: "stone".to_owned(),
            },
            policy: LockedIdentity {
                name: provenance.policy.name.clone(),
                fingerprint: provenance.policy.root.sha256.clone(),
            },
            target: identity("x86_64"),
            profile: LockedIdentity {
                name: "default-x86_64".to_owned(),
                fingerprint: profile_aggregate_fingerprint(&provenance.profiles),
            },
            toolchain: identity("llvm"),
            builder: identity("boulder-executor-v1"),
        };

        let plan = DerivationPlan {
            schema_version: DERIVATION_PLAN_SCHEMA_VERSION,
            boulder_version: "0.26.6".to_owned(),
            boulder_fingerprint: "sha256:boulder".to_owned(),
            package: PackageIdentity {
                name: "demo".to_owned(),
                version: "1.2.3".to_owned(),
                source_release: 4,
                build_release: 5,
                homepage: "https://demo.invalid".to_owned(),
                licenses: vec!["Zlib".to_owned(), "MIT".to_owned()],
                architecture: "x86_64".to_owned(),
            },
            source_lock_digest: provenance.recipe.explicit_inputs_sha256.clone(),
            provenance,
            sources: vec![
                LockedSource::Archive {
                    order: 0,
                    url: "https://src.invalid/demo.tar.xz".to_owned(),
                    sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                    filename: "demo.tar.xz".to_owned(),
                },
                LockedSource::Git {
                    order: 1,
                    url: "https://git.invalid/demo".to_owned(),
                    requested_ref: "v1.2.3".to_owned(),
                    commit: "1111111111111111111111111111111111111111".to_owned(),
                    directory: "demo-git".to_owned(),
                },
            ],
            build_lock: lock,
            jobs: vec![JobPlan {
                pgo_stage: Some("one".to_owned()),
                pgo_dir: Some("/sandbox/build/pgo".to_owned()),
                build_dir: "/sandbox/build/job".to_owned(),
                work_dir: "/sandbox/build/job/work".to_owned(),
                phases: vec![PhasePlan {
                    name: "build".to_owned(),
                    pre: vec![StepPlan::Run {
                        program: "prepare".to_owned(),
                        args: vec!["--first".to_owned(), "second value".to_owned()],
                        environment: BTreeMap::from([
                            ("Z_PRE".to_owned(), "z".to_owned()),
                            ("A_PRE".to_owned(), "a".to_owned()),
                        ]),
                        working_dir: "/sandbox/build/job/work".to_owned(),
                    }],
                    steps: vec![StepPlan::Shell {
                        interpreter: "/bin/sh".to_owned(),
                        script: "printf 'build\\n'".to_owned(),
                        environment: BTreeMap::from([("BUILD_MODE".to_owned(), "release".to_owned())]),
                        working_dir: "/sandbox/build/job/work".to_owned(),
                    }],
                    post: vec![StepPlan::Run {
                        program: "finish".to_owned(),
                        args: Vec::new(),
                        environment: BTreeMap::new(),
                        working_dir: "/sandbox/build/job".to_owned(),
                    }],
                }],
            }],
            environment: BTreeMap::from([
                ("Z_GLOBAL".to_owned(), "z".to_owned()),
                ("A_GLOBAL".to_owned(), "a".to_owned()),
            ]),
            layout: BuilderLayout {
                hostname: "sandbox-test".to_owned(),
                guest_root: "/sandbox".to_owned(),
                artifacts_dir: "/sandbox/artifacts".to_owned(),
                build_dir: "/sandbox/build".to_owned(),
                source_dir: "/sandbox/sources".to_owned(),
                recipe_dir: "/sandbox/recipe".to_owned(),
                install_dir: "/sandbox/install".to_owned(),
                package_dir: "/sandbox/recipe/pkg".to_owned(),
                ccache_dir: "/sandbox/cache/ccache".to_owned(),
                sccache_dir: "/sandbox/cache/sccache".to_owned(),
                go_cache_dir: "/sandbox/cache/go-build".to_owned(),
                go_mod_cache_dir: "/sandbox/cache/go-mod".to_owned(),
                cargo_cache_dir: "/sandbox/cache/cargo".to_owned(),
                zig_cache_dir: "/sandbox/cache/zig".to_owned(),
            },
            execution: ExecutionPolicy {
                network: NetworkMode::Disabled,
                filesystems: Default::default(),
                compiler_cache: true,
                jobs: 8,
            },
            analysis: AnalysisPlan {
                handlers: vec![AnalyzerKind::Elf, AnalyzerKind::Binary, AnalyzerKind::IncludeAny],
                toolchain: AnalysisToolchain::Gnu,
                debug: true,
                strip: false,
                compress_man: true,
                remove_libtool: false,
            },
            manifest_build_inputs: vec![
                RelationPlan {
                    kind: RelationKind::PackageName,
                    name: "zlib-devel".to_owned(),
                },
                RelationPlan {
                    kind: RelationKind::Binary,
                    name: "cmake".to_owned(),
                },
            ],
            collection_rules: vec![
                CollectionRulePlan {
                    output: "out".to_owned(),
                    kind: PathRuleKind::Executable,
                    pattern: "usr/bin/*".to_owned(),
                },
                CollectionRulePlan {
                    output: "dev".to_owned(),
                    kind: PathRuleKind::Any,
                    pattern: "usr/include/**".to_owned(),
                },
            ],
            outputs: vec![
                OutputPlan {
                    name: "dev".to_owned(),
                    package_name: "demo-devel".to_owned(),
                    include_in_manifest: true,
                    summary: None,
                    description: Some("Development files".to_owned()),
                    provides_exclude: Vec::new(),
                    runtime_exclude: Vec::new(),
                    runtime_inputs: vec![OutputRelation::Planned {
                        output: "out".to_owned(),
                    }],
                    conflicts: Vec::new(),
                },
                OutputPlan {
                    name: "out".to_owned(),
                    package_name: "demo".to_owned(),
                    include_in_manifest: true,
                    summary: Some("Demo summary".to_owned()),
                    description: None,
                    provides_exclude: vec!["z-pattern".to_owned(), "a-pattern".to_owned()],
                    runtime_exclude: vec!["z-runtime".to_owned(), "a-runtime".to_owned()],
                    runtime_inputs: vec![OutputRelation::Locked {
                        relation: RelationPlan {
                            kind: RelationKind::Binary,
                            name: "alpha".to_owned(),
                        },
                        reference: LockedOutputRef {
                            package_id: "alpha-id".to_owned(),
                            output: "out".to_owned(),
                        },
                    }],
                    conflicts: vec![
                        RelationPlan {
                            kind: RelationKind::PackageName,
                            name: "z-conflict".to_owned(),
                        },
                        RelationPlan {
                            kind: RelationKind::Binary,
                            name: "a-conflict".to_owned(),
                        },
                    ],
                },
            ],
            source_date_epoch: 1_700_000_000,
        };

        plan.validate()
            .expect("explanation fixture must be a valid frozen plan");
        Fixture { plan }
    }

    #[test]
    fn complete_explanation_matches_the_golden() {
        assert_eq!(
            fixture().render(),
            include_str!("../../../tests/golden/recipe-explain.txt")
        );
    }

    #[test]
    fn unordered_categories_are_sorted_without_reordering_authored_sequences() {
        let first = fixture();
        let mut second = fixture();
        second.plan.package.licenses.reverse();
        second.plan.sources.reverse();
        second.plan.build_lock.repositories.reverse();
        second.plan.build_lock.requests.reverse();
        second.plan.build_lock.packages.reverse();
        for package in &mut second.plan.build_lock.packages {
            package.outputs.reverse();
            package.dependencies.reverse();
        }
        second.plan.manifest_build_inputs.reverse();
        second.plan.outputs.reverse();
        for output in &mut second.plan.outputs {
            output.provides_exclude.reverse();
            output.runtime_exclude.reverse();
            output.runtime_inputs.reverse();
            output.conflicts.reverse();
        }

        assert_eq!(first.render(), second.render());

        second.plan.analysis.handlers.swap(0, 1);
        assert_ne!(
            first.render(),
            second.render(),
            "analyzer handler precedence must remain visible"
        );
        second.plan.analysis.handlers.swap(0, 1);

        second.plan.collection_rules.reverse();
        assert_ne!(
            first.render(),
            second.render(),
            "collector matching precedence must remain visible"
        );
        second.plan.collection_rules.reverse();

        second.plan.provenance.profiles.swap(0, 1);
        assert_ne!(
            first.render(),
            second.render(),
            "profile fragment precedence must remain visible"
        );
        second.plan.provenance.profiles.swap(0, 1);

        second.plan.provenance.policy.layers.swap(1, 2);
        assert_ne!(
            first.render(),
            second.render(),
            "policy layer order, including empty layers, must remain visible"
        );
    }
}
