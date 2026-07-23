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
}

#[test]
fn minimal_dev_private_contract_uses_exact_linux_character_device_identities() {
    use crate::private_devices::{PRIVATE_DEVICE_ORDER, PrivateDevice};

    assert_eq!(
        PRIVATE_DEVICE_ORDER.map(|device| (device.name().to_bytes(), device.major(), device.minor())),
        [
            (b"null".as_slice(), 1, 3),
            (b"zero".as_slice(), 1, 5),
            (b"full".as_slice(), 1, 7),
        ]
    );
    assert_eq!(
        PRIVATE_DEVICE_ORDER,
        [PrivateDevice::Null, PrivateDevice::Zero, PrivateDevice::Full]
    );
}
