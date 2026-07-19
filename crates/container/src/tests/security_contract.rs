#[test]
fn namespace_capability_denial_is_not_inferred_from_generic_nix_failures() {
    for source in [Errno::EPERM, Errno::EACCES, Errno::ENOSYS] {
        assert!(host_denied_user_namespace_setup(&ContainerRunError::CloneNamespaces {
            source,
        }));
    }
    assert!(!host_denied_user_namespace_setup(&ContainerRunError::CloneNamespaces {
        source: Errno::EAGAIN,
    }));
    assert!(!host_denied_user_namespace_setup(&ContainerRunError::Nix {
        source: Errno::EPERM,
    }));
}

#[test]
fn execution_capability_classifier_accepts_only_known_host_admission_denials() {
    for source in [Errno::EPERM, Errno::EACCES, Errno::ENOSYS] {
        assert!(ContainerRunError::CloneNamespaces { source }.execution_capability_unavailable());
    }
    for code in [nix::libc::EPERM, nix::libc::EACCES, nix::libc::ENOSYS, nix::libc::E2BIG] {
        assert!(
            ContainerRunError::CloneIntoCgroup {
                source: io::Error::from_raw_os_error(code),
            }
            .execution_capability_unavailable()
        );
    }
    for operation in [
        "clear inherited supplementary groups",
        "normalize payload real, effective, and saved-set GIDs",
        "normalize payload real, effective, and saved-set UIDs",
        "clone descriptor-backed root mount for anchored root /tmp/root",
        "clone descriptor-backed bind mount for anchored source /tmp/source",
        "attach descriptor-backed root mount for anchored root /tmp/root",
        "mount /",
        "pivot_root",
        "sethostname",
        "unmount old root",
    ] {
        assert!(
            ContainerRunError::Failure {
                message: format!("{operation}: EPERM: Operation not permitted"),
            }
            .execution_capability_unavailable(),
            "rejected {operation}"
        );
    }
    assert!(
        ContainerRunError::Idmap {
            source: super::idmap::Error::MissingSubgid {
                uid: 1000,
                username: "builder".to_owned(),
            },
        }
        .execution_capability_unavailable()
    );
}

#[test]
fn execution_capability_classifier_does_not_soften_unrelated_permission_errors() {
    assert!(!ContainerRunError::CloneNamespaces { source: Errno::EAGAIN }.execution_capability_unavailable());
    assert!(!ContainerRunError::Nix { source: Errno::EPERM }.execution_capability_unavailable());
    assert!(
        !ContainerRunError::Failure {
            message: "run: EPERM: Operation not permitted".to_owned(),
        }
        .execution_capability_unavailable()
    );
    assert!(
        !ContainerRunError::Failure {
            message: "mount /: EIO: Input/output error".to_owned(),
        }
        .execution_capability_unavailable()
    );
    assert!(
        !ContainerRunError::Idmap {
            source: super::idmap::Error::VerifyUidMap {
                source: io::Error::from_raw_os_error(nix::libc::EPERM),
            },
        }
        .execution_capability_unavailable()
    );
    assert!(
        !ContainerRunError::Idmap {
            source: super::idmap::Error::ReadCallerUserCredentials {
                source: super::credentials::CredentialSyscallError::Kernel(Errno::EPERM),
            },
        }
        .execution_capability_unavailable()
    );
    assert!(
        !ContainerRunError::ChildCleanup {
            cleanup: io::Error::from_raw_os_error(nix::libc::EPERM),
            pidfd: None,
        }
        .execution_capability_unavailable()
    );
}

#[test]
fn user_namespace_is_mandatory_for_rootful_and_rootless_callers() {
    for networking in [false, true] {
        let flags = namespace_flags(networking);
        assert!(flags.contains(nix::sched::CloneFlags::CLONE_NEWUSER));
        assert_eq!(flags.contains(nix::sched::CloneFlags::CLONE_NEWNET), !networking);
    }
}

#[test]
fn minimal_dev_has_an_exact_non_entropy_device_set() {
    assert_eq!(MINIMAL_DEV_NODES, ["null", "zero", "full"]);
    assert_eq!(MINIMAL_DEV_IDENTITIES, [("null", 1, 3), ("zero", 1, 5), ("full", 1, 7)]);
}

#[test]
fn minimal_dev_accepts_only_exact_linux_character_device_identities() {
    for &(name, major, minor) in MINIMAL_DEV_IDENTITIES {
        let path = Path::new("/dev").join(name);
        let device = open_path_file(&path);
        validate_minimal_device_source(device.as_raw_fd(), &path, major, minor).unwrap();
    }

    let regular = tempfile::NamedTempFile::new().unwrap();
    let regular = open_path_file(regular.path());
    assert!(matches!(
        validate_minimal_device_source(regular.as_raw_fd(), Path::new("regular"), 1, 3),
        Err(ContainerError::UnsupportedAnchoredMountSource { mode, .. }) if mode == nix::libc::S_IFREG
    ));

    for (source, label, expected_major, expected_minor, actual_major, actual_minor) in [
        ("/dev/zero", "/dev/null", 1, 3, 1, 5),
        ("/dev/full", "/dev/zero", 1, 5, 1, 7),
        ("/dev/null", "/dev/full", 1, 7, 1, 3),
    ] {
        let wrong_device = open_path_file(Path::new(source));
        assert!(matches!(
            validate_minimal_device_source(
                wrong_device.as_raw_fd(),
                Path::new(label),
                expected_major,
                expected_minor,
            ),
            Err(ContainerError::UnexpectedMinimalDeviceIdentity {
                expected_major: error_expected_major,
                expected_minor: error_expected_minor,
                actual_major: error_actual_major,
                actual_minor: error_actual_minor,
                ..
            }) if (error_expected_major, error_expected_minor, error_actual_major, error_actual_minor)
                == (expected_major, expected_minor, actual_major, actual_minor)
        ));
    }
}
