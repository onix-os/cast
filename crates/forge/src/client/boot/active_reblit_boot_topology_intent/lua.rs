//! Lua adapter for the machine-local boot-topology intent (Phase L6).
//!
//! Decodes an authored Lua boot-topology declaration into raw selector strings
//! and the engine-neutral [`BootTargetInput`], then runs the *same*
//! `assemble_boot_topology` canonicalization and cross-checks the Gluon adapter
//! runs. Equivalent Gluon and Lua sources reach the identical validated intent
//! value; only the evaluation identity differs by engine.
//!
//! This is the evaluator-level adapter. Wiring `.lua` into the fixed retained
//! `etc/cast/boot-topology.*` path (with its `.glu`-specific revalidation
//! contract) is a separate, security-sensitive step tracked for later.

use declarative_config::{EvaluationDeadline, Source};
use lua_config::LuaEngine;
use serde::Deserialize;

use super::gluon::{assemble_boot_topology, BootTargetInput};
use super::{ActiveReblitBootTopologyIntentError, ActiveReblitBootTopologyIntentValue};

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

/// Stateless Lua adapter for the boot-topology declaration.
#[derive(Debug, Clone, Default)]
pub(super) struct LuaBootTopologyIntentEvaluator {
    engine: LuaEngine,
}

impl LuaBootTopologyIntentEvaluator {
    /// Decode and validate an authored Lua boot-topology source.
    ///
    /// Any Lua evaluation failure is surfaced through the shared evaluation
    /// contract error so a malformed source is rejected exactly like the Gluon
    /// path rejects one.
    fn evaluate(
        &self,
        source: &Source,
    ) -> Result<ActiveReblitBootTopologyIntentValue, ActiveReblitBootTopologyIntentError> {
        let deadline = EvaluationDeadline::start(self.engine.limits().timeout);
        let intent = self
            .engine
            .evaluate_within_as::<LuaBootTopologyIntent>(source, deadline)
            .map_err(|_| ActiveReblitBootTopologyIntentError::EvaluationContract {
                reason: "authored Lua boot-topology source did not evaluate to the intent schema",
            })?
            .value;
        assemble_boot_topology(intent.esp.partuuid, intent.esp.mount_point, intent.boot.into())
    }
}

#[cfg(test)]
mod tests {
    use super::super::gluon::gluon_value_for_test;
    use super::*;

    const ESP_PARTUUID: &str = "11111111-2222-3333-4444-555555555555";
    const XBOOTLDR_PARTUUID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    const ESP_MOUNT_POINT: &str = "/synthetic/esp-root";
    const XBOOTLDR_MOUNT_POINT: &str = "/synthetic/boot-root";

    fn lua_value(source: &str) -> ActiveReblitBootTopologyIntentValue {
        LuaBootTopologyIntentEvaluator::default()
            .evaluate(&Source::new("boot-topology.lua", source))
            .expect("lua boot topology evaluates")
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
        let gluon = gluon_value_for_test(ESP_PARTUUID, ESP_MOUNT_POINT, None)
            .expect("gluon alias intent converts");
        assert_eq!(lua_value(&alias_source()), gluon);
    }

    #[test]
    fn a_lua_distinct_intent_matches_the_gluon_conversion() {
        let gluon = gluon_value_for_test(
            ESP_PARTUUID,
            ESP_MOUNT_POINT,
            Some((XBOOTLDR_PARTUUID, XBOOTLDR_MOUNT_POINT)),
        )
        .expect("gluon distinct intent converts");
        assert_eq!(lua_value(&distinct_source()), gluon);
    }

    #[test]
    fn a_lua_intent_with_a_noncanonical_partuuid_is_rejected() {
        let source = alias_source().replace(ESP_PARTUUID, "NOT-A-CANONICAL-UUID");
        assert!(
            LuaBootTopologyIntentEvaluator::default()
                .evaluate(&Source::new("boot-topology.lua", &source))
                .is_err()
        );
    }
}
