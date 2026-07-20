const REQUIRED_EXECUTION_FIXTURES: [&str; 28] = [
    "autotools",
    "autotools-options",
    "cargo",
    "cargo-features",
    "cargo-vendored",
    "cmake",
    "custom",
    "daemon-generated",
    "desktop-integration",
    "external-test-vectors",
    "factory-override",
    "font-family",
    "generated-config",
    "generated-shell",
    "gettext-localization",
    "go-module",
    "header-only-library",
    "hooks-patch",
    "meson",
    "multiple-sources",
    "pgo-workload",
    "plugin-output",
    "post-install-smoke-test",
    "python-module",
    "relation-policy",
    "split",
    "system-integration-assets",
    "userspace-profile",
];
const EXECUTION_FIXTURE_SELECTOR_ENV: &str = "CAST_EXECUTION_FIXTURE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionFixtureSelection {
    All,
    One(&'static str),
}

impl ExecutionFixtureSelection {
    fn includes(self, fixture: &str) -> bool {
        match self {
            Self::All => true,
            Self::One(selected) => selected == fixture,
        }
    }

    fn expected_count(self) -> usize {
        match self {
            Self::All => REQUIRED_EXECUTION_FIXTURES.len(),
            Self::One(_) => 1,
        }
    }
}

fn parse_execution_fixture_selection(value: Option<&str>) -> Result<ExecutionFixtureSelection, String> {
    let value = value.unwrap_or("all");
    if value == "all" {
        return Ok(ExecutionFixtureSelection::All);
    }
    if let Some(fixture) = REQUIRED_EXECUTION_FIXTURES
        .iter()
        .copied()
        .find(|fixture| *fixture == value)
    {
        return Ok(ExecutionFixtureSelection::One(fixture));
    }
    Err(format!(
        "{EXECUTION_FIXTURE_SELECTOR_ENV} must be `all` or exactly one of {}; got {value:?}",
        REQUIRED_EXECUTION_FIXTURES.join(", ")
    ))
}

fn execution_fixture_selection_from_env() -> Result<ExecutionFixtureSelection, String> {
    let Some(value) = std::env::var_os(EXECUTION_FIXTURE_SELECTOR_ENV) else {
        return parse_execution_fixture_selection(None);
    };
    let value = value.to_str().ok_or_else(|| {
        format!("{EXECUTION_FIXTURE_SELECTOR_ENV} must contain valid UTF-8 and name exactly one fixture or `all`")
    })?;
    parse_execution_fixture_selection(Some(value))
}
