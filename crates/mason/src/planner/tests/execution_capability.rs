#[test]
fn frozen_execution_capability_skip_never_hides_payload_or_ambiguous_nix_failures() {
    let missing_delegation = crate::container::Error::FrozenCgroupDelegationRequired {
        current: PathBuf::from("/user.slice/session.scope"),
    };
    assert!(container_capability_unavailable(&missing_delegation));
    let malformed_delegation = crate::container::Error::MalformedCurrentCgroup {
        reason: "duplicate unified entry",
    };
    assert!(!container_capability_unavailable(&malformed_delegation));

    for source in [
        nix::errno::Errno::EPERM,
        nix::errno::Errno::EACCES,
        nix::errno::Errno::ENOSYS,
    ] {
        let namespace = crate::container::Error::Container(::container::Error::CloneNamespaces { source });
        assert!(container_capability_unavailable(&namespace));
    }
    let namespace_resource_exhaustion = crate::container::Error::Container(::container::Error::CloneNamespaces {
        source: nix::errno::Errno::EAGAIN,
    });
    assert!(!container_capability_unavailable(&namespace_resource_exhaustion));

    for operation in [
        "clear inherited supplementary groups",
        "normalize payload real, effective, and saved-set GIDs",
        "normalize payload real, effective, and saved-set UIDs",
        "mount /work",
    ] {
        for terminal in [
            "EPERM: Operation not permitted",
            "EACCES: Permission denied",
            "ENOSYS: Function not implemented",
        ] {
            let setup = crate::container::Error::Container(::container::Error::Failure {
                message: format!("{operation}: {terminal}"),
            });
            assert!(container_capability_unavailable(&setup));
        }
    }

    for message in [
        "mount /work/EPERM: EIO: Input/output error",
        "mount /work/EACCES: unrelated failure",
        "mount /work/ENOSYS: operation failed",
        "mount /work: Operation not permitted",
        "clear inherited supplementary groups: permission denied by payload text",
        "restrict payload scheduler to the fair class: EPERM: Operation not permitted",
        "drop all payload capabilities: EPERM: Operation not permitted",
        "install mandatory payload seccomp policy: EACCES: Permission denied",
    ] {
        let injected = crate::container::Error::Container(::container::Error::Failure {
            message: message.to_owned(),
        });
        assert!(
            !container_capability_unavailable(&injected),
            "diagnostic text must not classify {message:?} as a host capability denial"
        );
    }

    let payload = crate::container::Error::Container(::container::Error::Failure {
        message: "run: package frozen example: permission denied".to_owned(),
    });
    assert!(!container_capability_unavailable(&payload));

    let ambiguous = crate::container::Error::Container(::container::Error::Nix {
        source: nix::errno::Errno::EPERM,
    });
    assert!(!container_capability_unavailable(&ambiguous));

    let child_cleanup = crate::container::Error::Container(::container::Error::ChildCleanup {
        cleanup: std::io::Error::other("EPERM: Operation not permitted"),
        pidfd: None,
    });
    assert!(!container_capability_unavailable(&child_cleanup));

    // Even a setup-shaped primary plus a permission-shaped cleanup diagnostic
    // remains a typed post-clone lifecycle violation. The display-string
    // fallback must never override that typed classification.
    let child_cleanup_after_setup = crate::container::Error::Container(::container::Error::ChildCleanupAfterFailure {
        primary: Box::new(::container::Error::Failure {
            message: "mount /work: EIO: Input/output error".to_owned(),
        }),
        cleanup: std::io::Error::other("EPERM: Operation not permitted"),
        pidfd: None,
    });
    assert!(!container_capability_unavailable(&child_cleanup_after_setup));
}
