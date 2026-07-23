use std::{
    io::{self, Write as _},
    os::fd::AsFd as _,
};

use super::{support::*, *};

#[test]
fn every_semantic_entry_rebinds_to_the_exact_retained_stone_owner() {
    let fixture = PackageCmdlineFixture::new(fixture_entries([
        ("lib/kernel/6.12/10-kernel.cmdline".to_owned(), b"kernel=yes".to_vec()),
        (
            "lib/kernel/cmdline.d/20-global.cmdline".to_owned(),
            b"global=yes".to_vec(),
        ),
    ]));
    let stone = fixture.ready();
    let prepared = PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline()).unwrap();
    assert!(std::ptr::eq(prepared.source_owner, &stone));

    for entry in prepared.entries() {
        let asset = stone.asset_at(usize::from(entry.binding_index())).unwrap();
        assert_eq!(asset.state_id(), entry.state_id());
        assert_eq!(asset.logical_path().file_name(), Some(entry.filename()));
        assert_eq!(asset.digest(), entry.digest());
        assert_eq!(asset.length(), entry.length());
        match (entry.scope(), asset.role()) {
            (BoundActiveReblitPackageCmdlineScope::Global, BootAssetRole::GlobalCmdline) => {}
            (
                BoundActiveReblitPackageCmdlineScope::Kernel { version: expected },
                BootAssetRole::KernelCmdline { version: actual },
            ) => assert_eq!(expected, actual),
            pair => panic!("semantic scope and retained Stone role disagree: {pair:?}"),
        }
    }
    prepared.revalidate_until(future_deadline()).unwrap();
}

#[test]
fn substituted_binding_index_is_rejected_by_exact_coordinate_revalidation() {
    let fixture = PackageCmdlineFixture::new(fixture_entries([
        ("lib/kernel/6.12/10-first.cmdline".to_owned(), b"first=yes".to_vec()),
        ("lib/kernel/6.12/20-second.cmdline".to_owned(), b"second=yes".to_vec()),
    ]));
    let stone = fixture.ready();
    let mut prepared = PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline()).unwrap();
    let substituted = prepared.entries[1].binding_index;
    prepared.entries[0].binding_index = substituted;

    assert!(matches!(
        prepared.revalidate_until(future_deadline()),
        Err(ActiveReblitPackageCmdlineInputsError::SourceChanged { binding_index })
            if binding_index == usize::from(substituted)
    ));
}

#[test]
fn changed_digest_length_path_or_scope_is_rejected() {
    enum Mutation {
        Digest,
        Length,
        Path,
        Scope,
        StatePosition,
    }

    for mutation in [
        Mutation::Digest,
        Mutation::Length,
        Mutation::Path,
        Mutation::Scope,
        Mutation::StatePosition,
    ] {
        let fixture = one_global(b"stable=yes".as_slice());
        let stone = fixture.ready();
        let mut prepared = PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline()).unwrap();
        let binding_index = usize::from(prepared.entries[0].binding_index);
        match mutation {
            Mutation::Digest => prepared.entries[0].digest ^= 1,
            Mutation::Length => prepared.entries[0].length += 1,
            Mutation::Path => prepared.entries[0].logical_path.push("substituted"),
            Mutation::Scope => prepared.entries[0].scope = PackageCmdlineScope::Kernel { version: "6.12".into() },
            Mutation::StatePosition => prepared.entries[0].state_position = u16::MAX,
        }
        assert!(matches!(
            prepared.revalidate_until(future_deadline()),
            Err(ActiveReblitPackageCmdlineInputsError::SourceChanged { binding_index: actual })
                if actual == binding_index
        ));
    }
}

#[test]
fn changed_normalized_semantics_are_rejected_after_source_reauthentication() {
    let fixture = one_global(b"stable=yes".as_slice());
    let stone = fixture.ready();
    let mut prepared = PreparedActiveReblitPackageCmdlineInputs::prepare_until(&stone, future_deadline()).unwrap();
    let binding_index = usize::from(prepared.entries[0].binding_index);
    prepared.entries[0].snippet = "substituted=yes".into();

    assert!(matches!(
        prepared.revalidate_until(future_deadline()),
        Err(ActiveReblitPackageCmdlineInputsError::SourceChanged { binding_index: actual })
            if actual == binding_index
    ));
}

#[test]
fn digest_mismatch_and_short_explicit_offset_read_fail_closed() {
    let expected = xxhash_rust::xxh3::xxh3_128(b"expected");
    assert!(matches!(
        binding::require_digest(7, expected, b"substituted"),
        Err(ActiveReblitPackageCmdlineInputsError::DigestMismatch {
            binding_index: 7,
            expected: actual_expected,
            ..
        }) if actual_expected == expected
    ));

    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"abc").unwrap();
    let mut budget = PackageCmdlineBudget::new(PackageCmdlinePolicy::production(), future_deadline()).unwrap();
    assert!(matches!(
        binding::read_exact_source_at(file.as_fd(), 4, 9, &mut budget),
        Err(ActiveReblitPackageCmdlineInputsError::ReadSource {
            binding_index: 9,
            source,
        }) if source.kind() == io::ErrorKind::UnexpectedEof
    ));
}
