//! Lua declaration adapter for the trigger domain (Phase L2, private).
//!
//! This decodes an authored Lua trigger into the *same* shared
//! [`TriggerSpec`]/[`Trigger`] the Gluon adapter produces. Options and closed
//! variants use the Lua tagged encoding (`{ kind = "some", value = ... }`);
//! the conversion into the shared wire type mirrors the Gluon conversion
//! exactly, so equivalent Gluon and Lua sources normalize to equal domain
//! values. It is not yet registered for `.lua` discovery.

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation, EvaluationDeadline,
    EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::{LuaEngine, LuaOption};
use serde::Deserialize;

use crate::format::Trigger;
use crate::spec::{
    HandlerSpec, InhibitorsSpec, KeyValueSpec, PathDefinitionSpec, PathKindSpec,
    TriggerConversionError, TriggerSpec,
};

/// Stateful read-only Lua adapter for the trigger declaration boundary.
///
/// It decodes an authored Lua trigger into the shared [`TriggerSpec`] and runs
/// the same [`Trigger`] validation the Gluon adapter uses, so both engines
/// reach identical domain values with intentionally distinct evaluation
/// identities.
#[derive(Debug, Clone, Default)]
pub struct LuaTriggerEvaluator {
    engine: LuaEngine,
}

impl DeclarationEvaluator<Trigger> for LuaTriggerEvaluator {
    type Identity = EvaluationIdentity;
    type Error = TriggerConversionError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        Evaluation<Trigger, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_within_as::<LuaTriggerSpec>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let trigger = Trigger::try_from(TriggerSpec::from(evaluation.value))
            .map_err(DeclarationEvaluationError::conversion)?;
        Ok(Evaluation {
            value: trigger,
            identity: evaluation.identity,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LuaTriggerSpec {
    name: String,
    description: String,
    before: LuaOption<String>,
    after: LuaOption<String>,
    inhibitors: LuaOption<LuaInhibitorsSpec>,
    paths: Vec<LuaKeyValueSpec<LuaPathDefinitionSpec>>,
    handlers: Vec<LuaKeyValueSpec<LuaHandlerSpec>>,
}

#[derive(Debug, Clone, Deserialize)]
struct LuaInhibitorsSpec {
    paths: Vec<String>,
    environment: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LuaPathDefinitionSpec {
    handlers: Vec<String>,
    kind: LuaOption<LuaPathKindSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum LuaPathKindSpec {
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum LuaHandlerSpec {
    Run { command: String, args: Vec<String> },
    Delete { paths: Vec<String> },
}

#[derive(Debug, Clone, Deserialize)]
struct LuaKeyValueSpec<T> {
    key: String,
    value: T,
}

impl From<LuaTriggerSpec> for TriggerSpec {
    fn from(spec: LuaTriggerSpec) -> Self {
        Self {
            name: spec.name,
            description: spec.description,
            before: spec.before.into(),
            after: spec.after.into(),
            inhibitors: Option::<LuaInhibitorsSpec>::from(spec.inhibitors).map(Into::into),
            paths: spec.paths.into_iter().map(Into::into).collect(),
            handlers: spec.handlers.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<LuaInhibitorsSpec> for InhibitorsSpec {
    fn from(spec: LuaInhibitorsSpec) -> Self {
        Self {
            paths: spec.paths,
            environment: spec.environment,
        }
    }
}

impl From<LuaPathDefinitionSpec> for PathDefinitionSpec {
    fn from(spec: LuaPathDefinitionSpec) -> Self {
        Self {
            handlers: spec.handlers,
            kind: Option::<LuaPathKindSpec>::from(spec.kind).map(Into::into),
        }
    }
}

impl From<LuaPathKindSpec> for PathKindSpec {
    fn from(spec: LuaPathKindSpec) -> Self {
        match spec {
            LuaPathKindSpec::Directory => Self::Directory,
            LuaPathKindSpec::Symlink => Self::Symlink,
        }
    }
}

impl From<LuaHandlerSpec> for HandlerSpec {
    fn from(spec: LuaHandlerSpec) -> Self {
        match spec {
            LuaHandlerSpec::Run { command, args } => Self::Run { command, args },
            LuaHandlerSpec::Delete { paths } => Self::Delete { paths },
        }
    }
}

impl<T, U> From<LuaKeyValueSpec<T>> for KeyValueSpec<U>
where
    U: From<T>,
{
    fn from(entry: LuaKeyValueSpec<T>) -> Self {
        Self {
            key: entry.key,
            value: entry.value.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::{DeclarationEvaluator, Source};
    use lua_config::LuaEngine;

    use super::*;
    use crate::format::Trigger;
    use crate::gluon::GluonTriggerEvaluator;

    const GLUON_TRIGGER: &str = r#"
let cast = import! cast.trigger.v1
let base = cast.trigger "depmod" "Rebuild kernel module dependencies"
{
    paths = [
        cast.path "/usr/lib/modules/(version:*)" ["depmod"] cast.optional.unset,
    ],
    handlers = [
        cast.handler.named "depmod" (cast.handler.run "/usr/bin/depmod" ["$(version)"]),
    ],
    .. base
}
"#;

    const LUA_TRIGGER: &str = r#"
return {
    name = "depmod",
    description = "Rebuild kernel module dependencies",
    before = { kind = "none" },
    after = { kind = "none" },
    inhibitors = { kind = "none" },
    paths = {
        {
            key = "/usr/lib/modules/(version:*)",
            value = { handlers = { "depmod" }, kind = { kind = "none" } },
        },
    },
    handlers = {
        {
            key = "depmod",
            value = { kind = "run", command = "/usr/bin/depmod", args = { "$(version)" } },
        },
    },
}
"#;

    fn lua_trigger(source: &str) -> Trigger {
        let spec = LuaEngine::default()
            .evaluate_as::<LuaTriggerSpec>(&Source::new("trigger.lua", source))
            .expect("lua trigger evaluates")
            .value;
        Trigger::try_from(TriggerSpec::from(spec)).expect("lua trigger is valid")
    }

    fn gluon_trigger(source: &str) -> Trigger {
        <GluonTriggerEvaluator as DeclarationEvaluator<Trigger>>::evaluate(
            &GluonTriggerEvaluator::default(),
            &Source::new("trigger.glu", source),
        )
        .expect("gluon trigger evaluates")
        .value
    }

    #[test]
    fn a_lua_trigger_normalizes_to_the_same_domain_value_as_gluon() {
        // `Trigger` holds compiled `fnmatch` patterns with interior mutability
        // and so cannot derive `PartialEq`; compare the normalized debug form,
        // which reflects the immutable pattern text and every domain field.
        assert_eq!(
            format!("{:?}", lua_trigger(LUA_TRIGGER)),
            format!("{:?}", gluon_trigger(GLUON_TRIGGER)),
        );
    }

    #[test]
    fn the_paired_trigger_documentation_example_normalizes_equally() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let gluon = std::fs::read_to_string(format!("{root}/docs/examples/gluon/trigger.glu"))
            .expect("gluon trigger example");
        let lua = std::fs::read_to_string(format!("{root}/docs/examples/lua/trigger.lua"))
            .expect("lua trigger example");
        assert_eq!(
            format!("{:?}", lua_trigger(&lua)),
            format!("{:?}", gluon_trigger(&gluon)),
        );
    }

    #[test]
    fn the_lua_trigger_evaluator_matches_gluon_through_the_typed_boundary() {
        let evaluator = LuaTriggerEvaluator::default();
        let evaluation = <LuaTriggerEvaluator as DeclarationEvaluator<Trigger>>::evaluate(
            &evaluator,
            &Source::new("trigger.lua", LUA_TRIGGER),
        )
        .expect("lua trigger evaluator succeeds");
        assert_eq!(
            format!("{:?}", evaluation.value),
            format!("{:?}", gluon_trigger(GLUON_TRIGGER)),
        );
        assert_eq!(evaluation.identity.engine.implementation(), "lua");
    }

    #[test]
    fn the_lua_and_gluon_identities_differ_by_engine() {
        let lua = LuaEngine::default()
            .evaluate_as::<LuaTriggerSpec>(&Source::new("trigger.lua", LUA_TRIGGER))
            .unwrap();
        assert_eq!(lua.identity.engine.implementation(), "lua");
        assert_eq!(lua.identity.language.as_str(), "lua");
    }
}
