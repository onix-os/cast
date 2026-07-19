use std::{ffi::OsStr, os::fd::AsRawFd as _};

use nix::unistd::{Whence, lseek};

use super::{support::*, *};

#[test]
fn semantic_inputs_are_state_scoped_versioned_sorted_and_normalized() {
    let fixture = PackageCmdlineFixture::new(fixture_entries([
        (
            "lib/kernel/cmdline.d/20-global.cmdline".to_owned(),
            b"  global.second=yes\r\n".to_vec(),
        ),
        (
            "lib/kernel/6.12/20-kernel.cmdline".to_owned(),
            b" kernel.second=yes \n".to_vec(),
        ),
        (
            "lib/kernel/cmdline.d/10-global.cmdline".to_owned(),
            b" quiet\n\n# ignored\nsplash \n".to_vec(),
        ),
        (
            "lib/kernel/6.12/10-kernel.cmdline".to_owned(),
            b"module.first=yes\n# ignored\nmodule.last=yes".to_vec(),
        ),
    ]));
    let stone = fixture.ready();
    let prepared = PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline()).unwrap();
    let entries = prepared.entries().collect::<Vec<_>>();

    assert_eq!(prepared.projected_state_ids(), stone.state_ids());
    assert_eq!(entries.len(), 4);
    assert_eq!(
        entries
            .iter()
            .map(|entry| (entry.filename(), entry.version(), entry.snippet()))
            .collect::<Vec<_>>(),
        [
            (
                OsStr::new("10-kernel.cmdline"),
                Some("6.12"),
                "module.first=yes module.last=yes"
            ),
            (OsStr::new("20-kernel.cmdline"), Some("6.12"), "kernel.second=yes"),
            (OsStr::new("10-global.cmdline"), None, "quiet  splash"),
            (OsStr::new("20-global.cmdline"), None, "global.second=yes"),
        ]
    );
    assert!(entries.iter().all(|entry| entry.state_id() == fixture.head.id));
    assert!(entries.iter().all(|entry| entry.length() > 0));
    assert!(entries.iter().all(|entry| entry.digest() != 0));
    assert!(
        entries
            .windows(2)
            .all(|pair| pair[0].binding_index() < pair[1].binding_index())
    );
    assert_eq!(
        prepared.total_source_bytes(),
        entries.iter().map(|entry| entry.length() as usize).sum::<usize>()
    );
    prepared.revalidate_until(future_deadline()).unwrap();
}

#[test]
fn non_cmdline_assets_never_enter_semantic_inputs() {
    let fixture = PackageCmdlineFixture::new(fixture_entries([
        ("lib/kernel/6.12/boot.initrd".to_owned(), b"initrd".to_vec()),
        ("lib/kernel/6.12/config".to_owned(), b"config".to_vec()),
        ("lib/kernel/cmdline.d/README".to_owned(), b"not a command line".to_vec()),
    ]));
    let stone = fixture.ready();
    let prepared = PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline()).unwrap();
    assert_eq!(prepared.entries().len(), 0);
    assert_eq!(prepared.total_source_bytes(), 0);
}

#[test]
fn empty_and_comment_only_sources_are_retained_canonically() {
    let fixture = PackageCmdlineFixture::new(fixture_entries([
        ("lib/kernel/6.12/00-empty.cmdline".to_owned(), Vec::new()),
        (
            "lib/kernel/cmdline.d/00-comments.cmdline".to_owned(),
            b" # first\n# second\n".to_vec(),
        ),
    ]));
    let stone = fixture.ready();
    let prepared = PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline()).unwrap();
    let entries = prepared.entries().collect::<Vec<_>>();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.snippet().is_empty()));
    assert_eq!(entries[0].length(), 0);
    assert_eq!(entries[1].length(), b" # first\n# second\n".len() as u64);
}

#[test]
fn non_ascii_and_embedded_controls_fail_closed() {
    for (bytes, expected_reason) in [
        (
            "quiet café".as_bytes(),
            ActiveReblitPackageCmdlineContentReason::NonAsciiOrUnsupportedControl,
        ),
        (
            b"quiet\x1bunsafe".as_slice(),
            ActiveReblitPackageCmdlineContentReason::NonAsciiOrUnsupportedControl,
        ),
        (
            b"quiet\tunsafe".as_slice(),
            ActiveReblitPackageCmdlineContentReason::NormalizedControl,
        ),
    ] {
        let fixture = one_global(bytes);
        let stone = fixture.ready();
        assert!(matches!(
            PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline()),
            Err(ActiveReblitPackageCmdlineInputsError::InvalidContent { reason, .. })
                if reason == expected_reason
        ));
    }
}

#[test]
fn preparation_uses_explicit_offsets_without_moving_the_shared_snapshot_cursor() {
    let source = b"quiet explicit-offset=yes";
    let fixture = one_global(source.as_slice());
    let stone = fixture.ready();
    let asset = stone
        .assets()
        .find(|asset| matches!(asset.role(), BootAssetRole::GlobalCmdline))
        .unwrap();
    let descriptor = asset.descriptor().as_raw_fd();
    let end = i64::try_from(asset.length()).unwrap();
    assert_eq!(lseek(descriptor, end, Whence::SeekSet).unwrap(), end);
    drop(asset);

    let prepared = PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline()).unwrap();
    assert_eq!(
        prepared.entries().next().unwrap().snippet(),
        "quiet explicit-offset=yes"
    );
    assert_eq!(lseek(descriptor, 0, Whence::SeekCur).unwrap(), end);
    prepared.revalidate_until(future_deadline()).unwrap();
    assert_eq!(lseek(descriptor, 0, Whence::SeekCur).unwrap(), end);
}
