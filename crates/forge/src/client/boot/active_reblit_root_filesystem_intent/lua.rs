//! Lua adapter for the machine-local root-filesystem intent (Phase L6).
//!
//! Decodes an authored Lua root-filesystem declaration into the single root
//! locator string, then runs the *same* `materialize_root_argument` bounded
//! normalization the Gluon adapter runs — under the caller-owned budget, so the
//! byte-limit, work-reservation, and deadline authority is identical. Equivalent
//! Gluon and Lua sources reach the identical validated intent value.
//!
//! This is the evaluator-level adapter. Wiring `.lua` into the fixed retained
//! `etc/cast/root-filesystem.*` path (with its `.glu`-specific revalidation
//! contract) is a separate, security-sensitive step tracked for later.

use declarative_config::{EvaluationDeadline, Source};
use lua_config::LuaEngine;
use serde::Deserialize;

use super::normalization::materialize_root_argument;
use super::{
    ActiveReblitRootFilesystemIntentError, RootFilesystemIntentBudget, RootFilesystemIntentValue,
};

#[derive(Debug, Clone, Deserialize)]
struct LuaRootFilesystemIntent {
    root: String,
}

/// Stateless Lua adapter for the root-filesystem declaration.
#[derive(Debug, Clone, Default)]
pub(super) struct LuaRootFilesystemIntentEvaluator {
    engine: LuaEngine,
}

impl LuaRootFilesystemIntentEvaluator {
    /// Decode and normalize an authored Lua root-filesystem source under the
    /// caller-established budget.
    fn evaluate(
        &self,
        source: &Source,
        budget: &mut RootFilesystemIntentBudget,
    ) -> Result<RootFilesystemIntentValue, ActiveReblitRootFilesystemIntentError> {
        let deadline = EvaluationDeadline::start(self.engine.limits().timeout);
        let intent = self
            .engine
            .evaluate_within_as::<LuaRootFilesystemIntent>(source, deadline)
            .map_err(|_| ActiveReblitRootFilesystemIntentError::EvaluationContract {
                reason: "authored Lua root-filesystem source did not evaluate to the intent schema",
            })?
            .value;
        materialize_root_argument(intent.root, budget)
    }
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

    #[test]
    fn a_lua_root_intent_matches_the_gluon_normalization() {
        let fixture = Fixture::new();
        let source = r#"return { root = "UUID=1111-2222" }"#;

        let lua = LuaRootFilesystemIntentEvaluator::default()
            .evaluate(&Source::new("root-filesystem.lua", source), &mut fixture.budget())
            .expect("lua root intent evaluates");
        let gluon = gluon_value_for_test("UUID=1111-2222", &mut fixture.budget())
            .expect("gluon root intent normalizes");

        assert_eq!(lua, gluon);
    }

    #[test]
    fn a_lua_root_intent_with_the_reserved_prefix_is_rejected() {
        let fixture = Fixture::new();
        let source = r#"return { root = "root=UUID=1111-2222" }"#;

        assert!(
            LuaRootFilesystemIntentEvaluator::default()
                .evaluate(&Source::new("root-filesystem.lua", source), &mut fixture.budget())
                .is_err()
        );
    }
}
