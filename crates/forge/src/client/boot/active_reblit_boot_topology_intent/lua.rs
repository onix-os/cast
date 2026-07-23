//! Lua adapter for the machine-local boot-topology intent (Phase L6).
//!
//! Decodes an authored Lua boot-topology declaration into raw selector strings
//! and the engine-neutral [`BootTargetInput`], then runs the *same*
//! `assemble_boot_topology` canonicalization and cross-checks the Gluon adapter
//! runs. Equivalent Gluon and Lua sources reach the identical validated intent
//! value; only the evaluation identity differs by engine.
//!
//! This is the budget-integrated adapter registered alongside the Gluon one, so
//! a retained `etc/cast/boot-topology.lua` is discovered by extension and
//! evaluated under the same absolute deadline and byte bounds. Its evaluation
//! contract mirrors the Gluon adapter's strictness: the fixed Lua source name,
//! no admitted external inputs, and — because the Lua boot declaration imports
//! nothing — an empty module set.

use std::time::Duration;

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation as DeclarationEvaluation,
    EvaluationDeadline, EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::LuaEngine;
use serde::Deserialize;

use super::gluon::{assemble_boot_topology, BootTargetInput, SOURCE_LOGICAL_NAME};
use super::{
    ActiveReblitBootTopologyIntentError, ActiveReblitBootTopologyIntentValue,
    BootTopologyIntentBudget,
};

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MAX_EVALUATION_TIME: Duration = Duration::from_secs(2);

/// Neutral language descriptor for the Lua boot-topology adapter, used to
/// register the `.lua` extension in the fixed-path discovery slot.
pub(super) fn language_spec() -> LanguageSpec {
    LuaEngine::default().language_spec().clone()
}

#[derive(Debug, Clone, Deserialize)]
struct LuaPartitionSelector {
    partuuid: String,
    mount_point: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LuaBootTarget {
    AliasEsp,
    DistinctXbootldr { xbootldr: LuaPartitionSelector },
}

#[derive(Debug, Clone, Deserialize)]
struct LuaBootTopologyIntent {
    esp: LuaPartitionSelector,
    boot: LuaBootTarget,
}

impl From<LuaBootTarget> for BootTargetInput {
    fn from(target: LuaBootTarget) -> Self {
        match target {
            LuaBootTarget::AliasEsp => Self::AliasEsp,
            LuaBootTarget::DistinctXbootldr { xbootldr } => Self::DistinctXbootldr {
                partuuid: xbootldr.partuuid,
                mount_point: xbootldr.mount_point,
            },
        }
    }
}

/// Budget-integrated Lua adapter for the closed boot-topology declaration.
///
/// Like the Gluon adapter it borrows the caller-owned absolute budget so the
/// typed evaluation boundary cannot replace ActiveReblit's deadline with a fresh
/// relative timeout.
pub(super) struct LuaBootTopologyIntentEvaluator<'budget> {
    engine: LuaEngine,
    budget: &'budget BootTopologyIntentBudget,
}

impl<'budget> LuaBootTopologyIntentEvaluator<'budget> {
    pub(super) fn new(
        budget: &'budget BootTopologyIntentBudget,
    ) -> Result<Self, ActiveReblitBootTopologyIntentError> {
        budget.require_deadline()?;
        let remaining = budget.remaining_duration()?;
        let mut limits = Limits::default();
        limits.max_source_bytes = budget.policy.max_source_bytes;
        limits.max_explicit_input_bytes = 0;
        // The Lua boot-topology declaration is self-contained: it imports no ABI.
        limits.max_imported_file_bytes = 0;
        limits.max_imports = 0;
        limits.max_import_graph_bytes = budget.policy.max_source_bytes;
        limits.timeout = remaining.min(MAX_EVALUATION_TIME);

        Ok(Self {
            engine: LuaEngine::new(limits),
            budget,
        })
    }
}

impl DeclarationEvaluator<ActiveReblitBootTopologyIntentValue>
    for LuaBootTopologyIntentEvaluator<'_>
{
    type Identity = EvaluationIdentity;
    type Error = ActiveReblitBootTopologyIntentError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
            budget: self.budget,
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<ActiveReblitBootTopologyIntentValue, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_within_as::<LuaBootTopologyIntent>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        self.budget
            .require_deadline()
            .map_err(DeclarationEvaluationError::Conversion)?;
        require_lua_fingerprint_contract(&evaluation.identity)
            .map_err(DeclarationEvaluationError::Conversion)?;

        let intent = evaluation.value;
        let value = assemble_boot_topology(intent.esp.partuuid, intent.esp.mount_point, intent.boot.into())
            .map_err(DeclarationEvaluationError::Conversion)?;
        self.budget
            .require_deadline()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value,
            identity: evaluation.identity,
        })
    }
}

fn require_lua_fingerprint_contract(
    fingerprint: &EvaluationIdentity,
) -> Result<(), ActiveReblitBootTopologyIntentError> {
    fingerprint.validate()?;
    // The fixed slot has one canonical logical name regardless of engine; the
    // `.lua` source binds the same slot identity as the `.glu` one.
    if fingerprint.root_logical_name != SOURCE_LOGICAL_NAME {
        return Err(ActiveReblitBootTopologyIntentError::EvaluationContract {
            reason: "evaluation fingerprint does not bind the fixed topology-intent slot name",
        });
    }
    if fingerprint.explicit_inputs_sha256 != EMPTY_SHA256 {
        return Err(ActiveReblitBootTopologyIntentError::EvaluationContract {
            reason: "boot-topology evaluation admitted explicit external inputs",
        });
    }
    if !fingerprint.modules.is_empty() {
        return Err(ActiveReblitBootTopologyIntentError::EvaluationContract {
            reason: "the Lua boot-topology declaration must import nothing",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, Instant};

    use super::super::gluon::gluon_value_for_test;
    use super::super::{BootTopologyIntentBudget, BootTopologyIntentPolicy};
    use super::*;
    use crate::Installation;

    const ESP_PARTUUID: &str = "11111111-2222-3333-4444-555555555555";
    const XBOOTLDR_PARTUUID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const ESP_MOUNT_POINT: &str = "/synthetic/esp-root";
    const XBOOTLDR_MOUNT_POINT: &str = "/synthetic/boot-root";

    struct Fixture {
        _temporary: tempfile::TempDir,
        installation: Installation,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("root");
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
            let installation = Installation::open(&root, None).unwrap();
            Self {
                _temporary: temporary,
                installation,
            }
        }

        fn budget(&self) -> BootTopologyIntentBudget {
            BootTopologyIntentBudget::new_until(
                &self.installation,
                BootTopologyIntentPolicy::production(),
                Instant::now() + Duration::from_secs(30),
            )
            .expect("budget admits within its deadline")
        }
    }

    fn lua_value(fixture: &Fixture, source: &str) -> ActiveReblitBootTopologyIntentValue {
        let budget = fixture.budget();
        LuaBootTopologyIntentEvaluator::new(&budget)
            .expect("lua evaluator admits")
            .evaluate(&Source::new(SOURCE_LOGICAL_NAME, source))
            .expect("lua boot topology evaluates")
            .value
    }

    fn alias_source() -> String {
        format!(
            r#"
return {{
    esp = {{ partuuid = "{ESP_PARTUUID}", mount_point = "{ESP_MOUNT_POINT}" }},
    boot = {{ kind = "alias_esp" }},
}}
"#
        )
    }

    fn distinct_source() -> String {
        format!(
            r#"
return {{
    esp = {{ partuuid = "{ESP_PARTUUID}", mount_point = "{ESP_MOUNT_POINT}" }},
    boot = {{
        kind = "distinct_xbootldr",
        xbootldr = {{ partuuid = "{XBOOTLDR_PARTUUID}", mount_point = "{XBOOTLDR_MOUNT_POINT}" }},
    }},
}}
"#
        )
    }

    #[test]
    fn a_lua_alias_intent_matches_the_gluon_conversion() {
        let fixture = Fixture::new();
        let gluon = gluon_value_for_test(ESP_PARTUUID, ESP_MOUNT_POINT, None)
            .expect("gluon alias intent converts");
        assert_eq!(lua_value(&fixture, &alias_source()), gluon);
    }

    #[test]
    fn a_lua_distinct_intent_matches_the_gluon_conversion() {
        let fixture = Fixture::new();
        let gluon = gluon_value_for_test(
            ESP_PARTUUID,
            ESP_MOUNT_POINT,
            Some((XBOOTLDR_PARTUUID, XBOOTLDR_MOUNT_POINT)),
        )
        .expect("gluon distinct intent converts");
        assert_eq!(lua_value(&fixture, &distinct_source()), gluon);
    }

    #[test]
    fn a_lua_intent_with_a_noncanonical_partuuid_is_rejected() {
        let fixture = Fixture::new();
        let budget = fixture.budget();
        let source = alias_source().replace(ESP_PARTUUID, "NOT-A-CANONICAL-UUID");
        assert!(
            LuaBootTopologyIntentEvaluator::new(&budget)
                .expect("lua evaluator admits")
                .evaluate(&Source::new(SOURCE_LOGICAL_NAME, &source))
                .is_err()
        );
    }

    #[test]
    fn a_lua_intent_under_the_wrong_source_name_is_rejected_by_the_contract() {
        let fixture = Fixture::new();
        let budget = fixture.budget();
        assert!(
            LuaBootTopologyIntentEvaluator::new(&budget)
                .expect("lua evaluator admits")
                .evaluate(&Source::new("etc/cast/elsewhere.lua", &alias_source()))
                .is_err()
        );
    }
}
