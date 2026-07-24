//! Lua adapter for the machine-local root-filesystem intent (Phase L6).
//!
//! Decodes an authored Lua root-filesystem declaration into the single root
//! locator string, then runs the *same* `materialize_root_argument` bounded
//! normalization the Gluon adapter runs — under the caller-owned budget, so the
//! byte-limit, work-reservation, and deadline authority is identical. Equivalent
//! Gluon and Lua sources reach the identical validated intent value.
//!
//! This is the budget-integrated adapter registered alongside the Gluon one, so
//! a retained `etc/cast/root-filesystem.lua` is discovered by extension and
//! normalized under the same absolute deadline, byte limits, and work
//! reservation. Its evaluation contract mirrors the Gluon adapter's strictness:
//! the fixed slot name, no admitted external inputs, and — because the Lua root
//! declaration imports nothing — an empty module set.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;
use std::time::Duration;

use declarative_config::{
    DeclarationEvaluationError, DeclarationEvaluator, Evaluation as DeclarationEvaluation,
    EvaluationDeadline, EvaluationIdentity, LanguageSpec, Limits, Source, SourceRoot,
};
use lua_config::{GENERATED_LUA_MARKER, LuaEngine, lua_string};
use serde::Deserialize;

use super::gluon::SOURCE_LOGICAL_NAME;
use super::normalization::materialize_root_argument;
use super::{
    ActiveReblitRootFilesystemIntentError, RootFilesystemIntentBudget, RootFilesystemIntentValue,
};

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const MAX_EVALUATION_TIME: Duration = Duration::from_secs(2);

/// Neutral language descriptor for the Lua root-filesystem adapter, used to
/// register the `.lua` extension in the fixed-path discovery slot.
pub(super) fn language_spec() -> LanguageSpec {
    LuaEngine::default().language_spec().clone()
}

#[derive(Debug, Clone, Deserialize)]
struct LuaRootFilesystemIntent {
    root: String,
}

/// Budget-integrated Lua adapter for the closed root-filesystem declaration.
pub(super) struct LuaRootFilesystemIntentEvaluator<'budget> {
    engine: LuaEngine,
    budget: Rc<RefCell<&'budget mut RootFilesystemIntentBudget>>,
}

impl<'budget> LuaRootFilesystemIntentEvaluator<'budget> {
    pub(super) fn new(
        budget: &'budget mut RootFilesystemIntentBudget,
    ) -> Result<Self, ActiveReblitRootFilesystemIntentError> {
        budget.require_deadline()?;
        let remaining = budget.remaining_duration()?;
        let mut limits = Limits::default();
        limits.max_source_bytes = budget.policy.max_source_bytes;
        limits.max_explicit_input_bytes = 0;
        // The Lua root-filesystem declaration is self-contained: it imports no ABI.
        limits.max_imported_file_bytes = 0;
        limits.max_imports = 0;
        limits.max_import_graph_bytes = budget.policy.max_source_bytes;
        limits.timeout = remaining.min(MAX_EVALUATION_TIME);

        Ok(Self {
            engine: LuaEngine::new(limits),
            budget: Rc::new(RefCell::new(budget)),
        })
    }
}

impl DeclarationEvaluator<RootFilesystemIntentValue> for LuaRootFilesystemIntentEvaluator<'_> {
    type Identity = EvaluationIdentity;
    type Error = ActiveReblitRootFilesystemIntentError;

    fn language_spec(&self) -> &LanguageSpec {
        self.engine.language_spec()
    }

    fn limits(&self) -> Limits {
        self.engine.limits()
    }

    fn with_source_root(&self, source_root: SourceRoot) -> Self {
        Self {
            engine: self.engine.clone().with_source_root(source_root),
            budget: Rc::clone(&self.budget),
        }
    }

    fn evaluate_within(
        &self,
        source: &Source,
        deadline: EvaluationDeadline,
    ) -> Result<
        DeclarationEvaluation<RootFilesystemIntentValue, Self::Identity>,
        DeclarationEvaluationError<Self::Error>,
    > {
        let evaluation = self
            .engine
            .evaluate_within_as::<LuaRootFilesystemIntent>(source, deadline)
            .map_err(DeclarationEvaluationError::Evaluation)?;
        let mut budget = self.budget.borrow_mut();
        budget
            .require_deadline()
            .map_err(DeclarationEvaluationError::Conversion)?;
        require_lua_fingerprint_contract(&evaluation.identity)
            .map_err(DeclarationEvaluationError::Conversion)?;

        let value = materialize_root_argument(evaluation.value.root, &mut budget)
            .map_err(DeclarationEvaluationError::Conversion)?;
        budget
            .require_deadline()
            .map_err(DeclarationEvaluationError::Conversion)?;
        Ok(DeclarationEvaluation {
            value,
            identity: evaluation.identity,
        })
    }
}

/// Emit a normalized root-filesystem intent as generated-marked Lua source that
/// re-decodes through [`LuaRootFilesystemIntentEvaluator`] to the same value.
///
/// The normalization stores the locator verbatim (rejecting the reserved
/// `root=` prefix, non-graphic bytes, quotes, and backslashes), so this is
/// idempotent: emitting the validated locator and decoding it again yields the
/// same intent. Because `etc/cast/root-filesystem.glu` is an *authored*,
/// boot-critical slot, this is the canonical Lua an operator adopts as the
/// verified replacement — never an authority Cast switches on its own.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn encode_lua_root_filesystem(value: &RootFilesystemIntentValue) -> String {
    let mut output = String::from(GENERATED_LUA_MARKER);
    output.push_str("return {\n");
    writeln!(output, "    root = {},", lua_string(&value.root)).unwrap();
    output.push_str("}\n");
    output
}

fn require_lua_fingerprint_contract(
    fingerprint: &EvaluationIdentity,
) -> Result<(), ActiveReblitRootFilesystemIntentError> {
    fingerprint.validate()?;
    // The fixed slot has one canonical logical name regardless of engine.
    if fingerprint.root_logical_name != SOURCE_LOGICAL_NAME {
        return Err(ActiveReblitRootFilesystemIntentError::EvaluationContract {
            reason: "evaluation fingerprint does not bind the fixed root-filesystem slot name",
        });
    }
    if fingerprint.explicit_inputs_sha256 != EMPTY_SHA256 {
        return Err(ActiveReblitRootFilesystemIntentError::EvaluationContract {
            reason: "root-filesystem evaluation admitted explicit external inputs",
        });
    }
    if !fingerprint.modules.is_empty() {
        return Err(ActiveReblitRootFilesystemIntentError::EvaluationContract {
            reason: "the Lua root-filesystem declaration must import nothing",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, Instant};

    use super::super::gluon::gluon_value_for_test;
    use super::super::RootFilesystemIntentPolicy;
    use super::*;
    use crate::Installation;

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

        fn budget(&self) -> RootFilesystemIntentBudget {
            RootFilesystemIntentBudget::new_until(
                &self.installation,
                RootFilesystemIntentPolicy::production(),
                Instant::now() + Duration::from_secs(30),
            )
            .expect("budget admits within its deadline")
        }
    }

    fn lua_value(
        budget: &mut RootFilesystemIntentBudget,
        source: &str,
    ) -> Result<RootFilesystemIntentValue, ActiveReblitRootFilesystemIntentError> {
        LuaRootFilesystemIntentEvaluator::new(budget)?
            .evaluate(&Source::new(SOURCE_LOGICAL_NAME, source))
            .map(|evaluation| evaluation.value)
            .map_err(|error| match error {
                DeclarationEvaluationError::Evaluation(source) => {
                    ActiveReblitRootFilesystemIntentError::Evaluation(source)
                }
                DeclarationEvaluationError::Conversion(source) => source,
            })
    }

    #[test]
    fn a_lua_root_intent_matches_the_gluon_normalization() {
        let fixture = Fixture::new();

        let lua = lua_value(&mut fixture.budget(), r#"return { root = "UUID=1111-2222" }"#)
            .expect("lua root intent evaluates");
        let gluon = gluon_value_for_test("UUID=1111-2222", &mut fixture.budget())
            .expect("gluon root intent normalizes");

        assert_eq!(lua, gluon);
    }

    #[test]
    fn the_paired_root_filesystem_documentation_example_normalizes_equally() {
        let root_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let lua = std::fs::read_to_string(format!("{root_dir}/docs/examples/lua/root-filesystem.lua"))
            .expect("lua root-filesystem example");
        let fixture = Fixture::new();
        let locator = "PARTUUID=11111111-2222-3333-4444-555555555555";

        let lua_value = lua_value(&mut fixture.budget(), &lua).expect("lua example evaluates");
        let gluon_value =
            gluon_value_for_test(locator, &mut fixture.budget()).expect("gluon normalizes");
        assert_eq!(lua_value, gluon_value);
    }

    #[test]
    fn a_lua_root_intent_with_the_reserved_prefix_is_rejected() {
        let fixture = Fixture::new();
        assert!(lua_value(&mut fixture.budget(), r#"return { root = "root=UUID=1111-2222" }"#).is_err());
    }

    #[test]
    fn an_emitted_root_intent_re_decodes_to_the_same_value() {
        let fixture = Fixture::new();
        let original = lua_value(&mut fixture.budget(), r#"return { root = "UUID=abcd-1234" }"#)
            .expect("root intent evaluates");

        let emitted = encode_lua_root_filesystem(&original);
        assert!(emitted.starts_with(GENERATED_LUA_MARKER));

        let redecoded =
            lua_value(&mut fixture.budget(), &emitted).expect("emitted root intent re-decodes");
        assert_eq!(redecoded, original);
    }

    #[test]
    fn a_lua_root_intent_under_the_wrong_source_name_is_rejected_by_the_contract() {
        let fixture = Fixture::new();
        let mut budget = fixture.budget();
        assert!(
            LuaRootFilesystemIntentEvaluator::new(&mut budget)
                .expect("lua evaluator admits")
                .evaluate(&Source::new("etc/cast/elsewhere.lua", r#"return { root = "UUID=1" }"#))
                .is_err()
        );
    }
}
