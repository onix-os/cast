use super::{support::*, *};

#[test]
fn masked_package_root_and_cast_keys_are_rejected_before_masking() {
    for (snippet, key) in [
        ("root", "root"),
        ("cast.fstx", "cast.fstx"),
        ("root=\"forbidden\"", "root"),
        ("cast.fstx=bad\\value", "cast.fstx"),
    ] {
        let fixture = RenderFixture::new(
            StateSpec::one_kernel("6.12").with_cmdline("lib/kernel/cmdline.d/10-masked.cmdline", snippet.as_bytes()),
            Vec::new(),
        );
        fixture.mask_local("10-masked.cmdline");
        let stone = fixture.stone();
        let roots = fixture.roots(&stone);
        let prepared = prepare_static(&fixture, &stone, &roots);
        let local = fixture.local_policy();
        let root = fixture.root_intent();

        assert!(matches!(
            prepared.revalidate_until(
                &fixture.state_db,
                &fixture.layout_db,
                &fixture.installation,
                &local,
                &root,
                future_deadline(),
            ),
            Err(ActiveReblitBootRenderInputsError::ReservedCmdlineKey {
                origin: ActiveReblitCmdlineSource::Package { .. },
                key: actual,
            }) if actual == key
        ));

        let local_fixture = simple_fixture();
        local_fixture.write_local("10-reserved.cmdline", snippet);
        let stone = local_fixture.stone();
        let roots = local_fixture.roots(&stone);
        let prepared = prepare_static(&local_fixture, &stone, &roots);
        let local = local_fixture.local_policy();
        let root = local_fixture.root_intent();
        assert!(matches!(
            prepared.revalidate_until(
                &local_fixture.state_db,
                &local_fixture.layout_db,
                &local_fixture.installation,
                &local,
                &root,
                future_deadline(),
            ),
            Err(ActiveReblitBootRenderInputsError::ReservedCmdlineKey {
                origin: ActiveReblitCmdlineSource::LocalAppend { entry_index: 0 },
                key: actual,
            }) if actual == key
        ));
    }
}

#[test]
fn package_and_local_quotes_or_backslashes_fail_with_exact_origin() {
    for snippet in ["ordinary=\"quoted\"", "ordinary=back\\slash"] {
        let package_fixture = RenderFixture::new(
            StateSpec::one_kernel("6.12").with_cmdline("lib/kernel/cmdline.d/10-package.cmdline", snippet.as_bytes()),
            Vec::new(),
        );
        let stone = package_fixture.stone();
        let roots = package_fixture.roots(&stone);
        let prepared = prepare_static(&package_fixture, &stone, &roots);
        let local = package_fixture.local_policy();
        let root = package_fixture.root_intent();
        assert!(matches!(
            prepared.revalidate_until(
                &package_fixture.state_db,
                &package_fixture.layout_db,
                &package_fixture.installation,
                &local,
                &root,
                future_deadline(),
            ),
            Err(ActiveReblitBootRenderInputsError::InvalidCmdlineToken {
                origin: ActiveReblitCmdlineSource::Package { .. },
                reason: ActiveReblitCmdlineTokenReason::UnsupportedQuoteOrEscape,
            })
        ));

        let local_fixture = simple_fixture();
        local_fixture.write_local("10-local.cmdline", snippet);
        let stone = local_fixture.stone();
        let roots = local_fixture.roots(&stone);
        let prepared = prepare_static(&local_fixture, &stone, &roots);
        let local = local_fixture.local_policy();
        let root = local_fixture.root_intent();
        assert!(matches!(
            prepared.revalidate_until(
                &local_fixture.state_db,
                &local_fixture.layout_db,
                &local_fixture.installation,
                &local,
                &root,
                future_deadline(),
            ),
            Err(ActiveReblitBootRenderInputsError::InvalidCmdlineToken {
                origin: ActiveReblitCmdlineSource::LocalAppend { entry_index: 0 },
                reason: ActiveReblitCmdlineTokenReason::UnsupportedQuoteOrEscape,
            })
        ));
    }

    for invalid in ["root=bad value", "root=\"bad\"", "root=bad\\value", "root=bad\nvalue"] {
        assert!(matches!(
            cmdline::validate_root_argument(invalid),
            Err(ActiveReblitBootRenderInputsError::InvalidRootArgument)
        ));
    }

    for (snippet, reason) in [
        ("--", ActiveReblitCmdlineTokenReason::EndOfOptionsSeparator),
        ("=empty", ActiveReblitCmdlineTokenReason::EmptyKey),
        ("ordinary=bad\u{7f}", ActiveReblitCmdlineTokenReason::NonPrintableAscii),
    ] {
        assert!(matches!(
            cmdline::audit_snippet_for_test(
                snippet,
                ActiveReblitCmdlineSource::Package { binding_index: 7 },
                future_deadline(),
            ),
            Err(ActiveReblitBootRenderInputsError::InvalidCmdlineToken {
                origin: ActiveReblitCmdlineSource::Package { binding_index: 7 },
                reason: actual,
            }) if actual == reason
        ));
    }
}
