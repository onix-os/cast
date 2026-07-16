use semantic_fingerprint::{ExplicitInput, calculate};

const BASE_FILES: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"bin/cast\", \"crates/forge\", \"crates/mason\", \"crates/stone\"]\n",
    ),
    ("Cargo.lock", "version = 4\n"),
    ("flake.nix", "{ outputs = _: {}; }\n"),
    ("flake.lock", "{}\n"),
    ("Makefile", "build:\n\tcargo build\n"),
    ("bin/cast/Cargo.toml", "[package]\nname = \"cast\"\n"),
    ("bin/cast/src/main.rs", "fn main() {}\n"),
    ("crates/forge/Cargo.toml", "[package]\nname = \"forge\"\n"),
    ("crates/forge/src/lib.rs", "pub fn transact() {}\n"),
    ("crates/mason/Cargo.toml", "[package]\nname = \"mason\"\n"),
    ("crates/mason/src/lib.rs", "pub fn build() {}\n"),
    ("crates/mason/data/policy/default.glu", "{ target = \"native\" }\n"),
    ("crates/mason/data/policy/policy.glu", "[]\n"),
    ("crates/mason/data/policy/tuning/flags.glu", "[]\n"),
    ("crates/mason/data/policy/tuning/groups.glu", "[]\n"),
    ("crates/stone/Cargo.toml", "[package]\nname = \"stone\"\n"),
    ("crates/stone/build.rs", "fn main() {}\n"),
    ("crates/stone/src/lib.rs", "pub fn read() {}\n"),
    ("crates/stone/gluon/stone.glu", "{ format = 1 }\n"),
];

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn identify_test_executable(path: &Path, with_version: bool) -> tool_identity::ExecutableIdentity {
    tool_identity::identify(path.as_os_str(), None, path.parent().unwrap(), |resolved| {
        if !with_version {
            return Ok(None);
        }
        let output = Command::new(resolved).arg("--version").output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "test executable {} failed with {}",
                resolved.display(),
                output.status
            )));
        }
        Ok(Some(output.stdout))
    })
    .unwrap()
    .unwrap()
}

fn fixture(files: impl IntoIterator<Item = (&'static str, &'static str)>) -> TempDir {
    let temporary = tempfile::tempdir().unwrap();
    for (path, contents) in files {
        write(temporary.path(), path, contents);
    }
    temporary
}

fn base_fixture() -> TempDir {
    fixture(BASE_FILES.iter().copied())
}

fn context() -> Vec<ExplicitInput> {
    vec![
        ("env.HOST".to_owned(), b"x86_64-unknown-linux-gnu".to_vec()),
        ("env.PROFILE".to_owned(), b"release".to_vec()),
        ("env.TARGET".to_owned(), b"x86_64-unknown-linux-gnu".to_vec()),
        ("toolchain.rustc-vV".to_owned(), b"rustc fixture".to_vec()),
    ]
}

fn value(root: &Path) -> String {
    calculate(root, context()).unwrap().value().to_owned()
}

fn native_environment(values: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    native_environment_for("x86_64-unknown-linux-gnu", "x86_64-unknown-linux-gnu", values)
}

fn native_environment_for(host: &str, target: &str, values: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    native_environment_for_platform(host, target, "linux", "gnu", values)
}

fn native_environment_for_platform(
    host: &str,
    target: &str,
    target_os: &str,
    target_env: &str,
    values: &[(&str, &str)],
) -> Vec<(OsString, OsString)> {
    [
        ("HOST", host),
        ("TARGET", target),
        ("CARGO_CFG_TARGET_OS", target_os),
        ("CARGO_CFG_TARGET_ENV", target_env),
    ]
    .into_iter()
    .chain(values.iter().copied())
    .map(|(key, value)| (OsString::from(key), OsString::from(value)))
    .collect()
}

fn fake_command_identity(command: &[OsString], tool_revision: &str) -> tool_identity::CommandIdentity {
    let program = command
        .first()
        .map(|value| value.as_os_str().as_bytes())
        .unwrap_or_default();
    let basename = program.rsplit(|byte| *byte == b'/').next().unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(basename);
    hasher.update(tool_revision.as_bytes());
    let content_sha256 = hasher.finalize().into();
    let resolved_path = if program.contains(&b'/') {
        PathBuf::from(OsString::from_vec(program.to_vec()))
    } else {
        Path::new("/resolved/toolchain/bin").join(OsString::from_vec(program.to_vec()))
    };
    let mut version = basename.to_vec();
    version.extend_from_slice(b" version ");
    version.extend_from_slice(tool_revision.as_bytes());
    tool_identity::CommandIdentity::new(tool_identity::ExecutableIdentity::from_parts(
        resolved_path,
        content_sha256,
        Some(version),
    ))
}

fn native_context(values: &[(&str, &str)], tool_revision: &str, workspace: &Path) -> Vec<ExplicitInput> {
    native_context_with_probe(values, workspace, |_, command| {
        Ok(Some(fake_command_identity(command, tool_revision)))
    })
}

fn native_context_with_probe<F>(values: &[(&str, &str)], workspace: &Path, probe: F) -> Vec<ExplicitInput>
where
    F: FnMut(&str, &[OsString]) -> std::io::Result<Option<tool_identity::CommandIdentity>>,
{
    native_build_context::collect(native_environment(values), workspace, probe)
        .unwrap()
        .inputs()
}

fn native_value(root: &Path, values: &[(&str, &str)], tool_revision: &str, workspace: &Path) -> String {
    let mut inputs = context();
    inputs.extend(native_context(values, tool_revision, workspace));
    calculate(root, inputs).unwrap().value().to_owned()
}
