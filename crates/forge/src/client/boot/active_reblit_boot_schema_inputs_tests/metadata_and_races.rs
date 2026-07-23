use std::{
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _},
};

use super::*;

fn history_fixture() -> Fixture {
    Fixture::new(
        FixtureSchemaSource::OsInfo(valid_os_info("head-os", "Head OS", &[])),
        vec![FixtureSchemaSource::Generated(valid_os_release("history", "History"))],
    )
}

#[test]
fn unsafe_mode_and_extra_hardlink_are_structural_history_failures() {
    for hardlink in [false, true] {
        let fixture = history_fixture();
        let history = fixture.histories[0].id;
        let path = fixture.generated_path(history);
        if hardlink {
            fs::hard_link(&path, path.with_extension("extra-link")).unwrap();
        } else {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        }
        let prepared = fixture.prepare().unwrap();
        assert!(matches!(
            prepared.schemas.schema_for_state(history).unwrap().source(),
            ActiveReblitBootSchemaSourceBinding::GlobalFallback {
                failed_local: ActiveReblitBootSchemaFallbackReason::Structural(
                    ActiveReblitBootSchemaStructuralReason::UnsafeOsRelease,
                ),
                ..
            }
        ));
    }
}

#[test]
fn symlinked_generated_metadata_is_never_followed() {
    let fixture = history_fixture();
    let history = fixture.histories[0].id;
    let path = fixture.generated_path(history);
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("/dev/null", &path).unwrap();

    let prepared = fixture.prepare().unwrap();
    assert!(matches!(
        prepared.schemas.schema_for_state(history).unwrap().source(),
        ActiveReblitBootSchemaSourceBinding::GlobalFallback {
            failed_local: ActiveReblitBootSchemaFallbackReason::Structural(
                ActiveReblitBootSchemaStructuralReason::UnsafeOsRelease,
            ),
            ..
        }
    ));
}

#[test]
fn arbitrary_xattr_is_rejected_when_the_fixture_filesystem_supports_it() {
    let fixture = history_fixture();
    let history = fixture.histories[0].id;
    let path = fixture.generated_path(history);
    let encoded = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path, name and value remain live for setxattr.
    let result = unsafe {
        nix::libc::setxattr(
            encoded.as_ptr(),
            c"user.schema-test".as_ptr(),
            b"x".as_ptr().cast(),
            1,
            0,
        )
    };
    if result != 0 {
        let source = io::Error::last_os_error();
        if matches!(source.raw_os_error(), Some(nix::libc::EOPNOTSUPP | nix::libc::EPERM)) {
            eprintln!("skipping xattr assertion: {source}");
            return;
        }
        panic!("set schema test xattr: {source}");
    }

    let prepared = fixture.prepare().unwrap();
    assert!(matches!(
        prepared.schemas.schema_for_state(history).unwrap().source(),
        ActiveReblitBootSchemaSourceBinding::GlobalFallback {
            failed_local: ActiveReblitBootSchemaFallbackReason::Structural(
                ActiveReblitBootSchemaStructuralReason::ExtendedAttributes,
            ),
            ..
        }
    ));
}

#[test]
fn same_byte_name_replacement_during_read_is_not_admitted() {
    let bytes = valid_os_release("head-os", "Head OS");
    let fixture = Fixture::new(FixtureSchemaSource::Generated(bytes.clone()), Vec::new());
    let path = fixture.generated_path(fixture.head.id);
    arm_after_generated_read(move |_| {
        let parked = path.with_extension("parked");
        fs::rename(&path, parked).unwrap();
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    });

    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitBootSchemaInputsError::RequiredSchemaUnavailable {
            reason: ActiveReblitBootSchemaFallbackReason::Structural(
                ActiveReblitBootSchemaStructuralReason::ChangedDuringRead,
            ),
            ..
        })
    ));
}

#[test]
fn revalidation_rejects_replacement_after_successful_preparation() {
    let bytes = valid_os_release("head-os", "Head OS");
    let fixture = Fixture::new(FixtureSchemaSource::Generated(bytes.clone()), Vec::new());
    let prepared = fixture.prepare().unwrap();
    let path = fixture.generated_path(fixture.head.id);
    fs::rename(&path, path.with_extension("old")).unwrap();
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(matches!(
        prepared.revalidate(&fixture),
        Err(ActiveReblitBootSchemaInputsError::GeneratedSourceChanged { state })
            if state == i32::from(fixture.head.id)
    ));
}

#[test]
fn eio_is_operational_and_never_becomes_history_fallback() {
    let fixture = history_fixture();
    let history = fixture.histories[0].id;
    arm_generated_operational_fault(nix::libc::EIO);

    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitBootSchemaInputsError::GeneratedIo { state, source, .. })
            if state == i32::from(history) && source.raw_os_error() == Some(nix::libc::EIO)
    ));
}

#[test]
fn lib_replacement_inside_generated_name_walk_is_rejected() {
    let bytes = valid_os_release("head-os", "Head OS");
    let fixture = Fixture::new(FixtureSchemaSource::Generated(bytes.clone()), Vec::new());
    let lib = fixture.usr_path(fixture.head.id).join("lib");
    arm_after_generated_name_lib_open(move |_| {
        fs::rename(&lib, lib.with_extension("parked")).unwrap();
        fs::create_dir(&lib).unwrap();
        fs::set_permissions(&lib, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(lib.join("os-release"), &bytes).unwrap();
        fs::set_permissions(lib.join("os-release"), fs::Permissions::from_mode(0o644)).unwrap();
    });

    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitBootSchemaInputsError::RequiredSchemaUnavailable {
            reason: ActiveReblitBootSchemaFallbackReason::Structural(
                ActiveReblitBootSchemaStructuralReason::ChangedDuringRead,
            ),
            ..
        })
    ));
}

#[test]
fn file_replacement_inside_generated_name_walk_is_rejected() {
    let bytes = valid_os_release("head-os", "Head OS");
    let fixture = Fixture::new(FixtureSchemaSource::Generated(bytes.clone()), Vec::new());
    let path = fixture.generated_path(fixture.head.id);
    arm_after_generated_name_file_open(move |_| {
        fs::rename(&path, path.with_extension("parked")).unwrap();
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    });

    assert!(matches!(
        fixture.prepare(),
        Err(ActiveReblitBootSchemaInputsError::RequiredSchemaUnavailable {
            reason: ActiveReblitBootSchemaFallbackReason::Structural(
                ActiveReblitBootSchemaStructuralReason::ChangedDuringRead,
            ),
            ..
        })
    ));
}

#[test]
fn file_replacement_after_generated_revalidation_read_is_rejected() {
    let bytes = valid_os_release("head-os", "Head OS");
    let fixture = Fixture::new(FixtureSchemaSource::Generated(bytes.clone()), Vec::new());
    let prepared = fixture.prepare().unwrap();
    let path = fixture.generated_path(fixture.head.id);
    arm_after_generated_revalidation_read(move |_| {
        fs::rename(&path, path.with_extension("parked-after-revalidation")).unwrap();
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    });

    assert!(matches!(
        prepared.revalidate(&fixture),
        Err(ActiveReblitBootSchemaInputsError::GeneratedSourceChanged { state })
            if state == i32::from(fixture.head.id)
    ));
}
