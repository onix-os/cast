#[test]
fn root_source_sandbox_and_platform_semantics_are_rejected_early() {
    let mut policy = repository_policy_value();
    policy.build_root.base.push(policy.build_root.base[0].clone());
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::Duplicate { field, value })
            if field == "build_root.base" && value == "glibc-devel"
    ));

    for path in [
        "relative/build",
        "/",
        "/mason/../escape",
        "/mason/./build",
        "/mason//build",
        "/mason/build/",
    ] {
        let mut policy = repository_policy_value();
        policy.sandbox.build_dir = path.to_owned();
        assert!(matches!(
            policy.validate(),
            Err(BuildPolicyConversionError::InvalidGuestPath { field, value })
                if field == "sandbox.build_dir" && value == path
        ));
    }

    let mut policy = repository_policy_value();
    policy.sandbox.build_dir = "/tmp/build".to_owned();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::GuestPathOutsideRoot { field, value, guest_root })
            if field == "sandbox.build_dir" && value == "/tmp/build" && guest_root == "/mason"
    ));

    let too_long = "x".repeat(65);
    for hostname in ["", "-cast", "cast-", "bad host", "bad/host", too_long.as_str()] {
        let mut policy = repository_policy_value();
        policy.sandbox.hostname = hostname.to_owned();
        assert!(matches!(
            policy.validate(),
            Err(BuildPolicyConversionError::InvalidHostname { field, value })
                if field == "sandbox.hostname" && value == hostname
        ));
    }

    let mut policy = repository_policy_value();
    policy.build_root.compiler_cache.ccache_dir = policy.sandbox.build_dir.clone();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::OverlappingGuestPath {
            field,
            other_field,
            ..
        }) if field == "build_root.compiler_cache.ccache_dir" && other_field == "sandbox.build_dir"
    ));

    let mut policy = repository_policy_value();
    policy.sandbox.source_dir = format!("{}/sources", policy.sandbox.build_dir);
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::OverlappingGuestPath {
            field,
            other_field,
            ..
        }) if field == "sandbox.source_dir" && other_field == "sandbox.build_dir"
    ));

    let mut policy = repository_policy_value();
    policy.build_root.compiler_cache.zig_cache_dir = "/outside/zig".to_owned();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::GuestPathOutsideRoot { field, .. })
            if field == "build_root.compiler_cache.zig_cache_dir"
    ));

    let mut policy = repository_policy_value();
    policy.build_root.compiler_cache.sccache_dir = policy.build_root.compiler_cache.ccache_dir.clone();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::OverlappingGuestPath {
            field,
            other_field,
            ..
        }) if field == "build_root.compiler_cache.sccache_dir"
            && other_field == "build_root.compiler_cache.ccache_dir"
    ));

    let mut policy = repository_policy_value();
    policy.targets[0].target_platform.vendor = "unknown".to_owned();
    assert!(matches!(
        policy.validate(),
        Err(BuildPolicyConversionError::InvalidPlatformComponent { field, value })
            if field == "targets[0].target_platform.vendor" && value == "unknown"
    ));
}
