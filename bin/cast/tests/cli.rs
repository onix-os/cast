use std::process::Command;

#[test]
fn help_exposes_build_and_system_commands_without_internal_namespaces() {
    let output = Command::new(env!("CARGO_BIN_EXE_cast")).arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in ["build", "recipe", "install", "state", "sync"] {
        assert!(help.contains(command), "missing command {command} in:\n{help}");
    }
    assert!(!help.contains("cast mason"));
    assert!(!help.contains("cast forge"));
}

#[test]
fn version_identifies_only_cast() {
    let output = Command::new(env!("CARGO_BIN_EXE_cast"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    let version = String::from_utf8(output.stdout).unwrap();
    assert!(version.starts_with("cast "), "{version:?}");
}
