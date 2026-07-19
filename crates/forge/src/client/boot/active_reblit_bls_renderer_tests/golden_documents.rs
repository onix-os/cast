use super::*;

#[test]
fn golden_alias_plan_matches_pinned_loader_entry_payload_and_bootloader_bytes() {
    let deadline = support::future_deadline();
    let spec = support::StateSpec::one_kernel("6.12").with_kernel(
        support::KernelSpec::new("6.13")
            .with_initrd("20-extra.initrd", b"extra".as_slice())
            .with_initrd("10-base.initrd", b"base".as_slice()),
    );
    with_render_inputs!(
        support::RenderFixture::new(spec, Vec::new()),
        deadline,
        |fixture, inputs| {
            let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
            let topology = topology::alias_topology();
            let (plan, _) = fixture_plan(rendered, &topology);
            assert_eq!(generated_at(&plan, "loader/loader.conf"), b"default \"head*\"\n");

            let state = i32::from(fixture.head.id);
            let expected_first = format!(
                "title Render Head (6.12)\n\
                 linux /EFI/head/6.12/vmlinuz\n\n\
                 options root={} cast.fstx={}\n",
                support::ROOT_LOCATOR,
                state
            );
            assert_eq!(
                generated_at(&plan, &format!("loader/entries/head-6.12-{state}.conf")),
                expected_first.as_bytes()
            );
            let expected = format!(
                "title Render Head (6.13)\n\
                 linux /EFI/head/6.13/vmlinuz\n\n\
                 initrd /EFI/head/6.13/10-base.initrd\n\
                 initrd /EFI/head/6.13/20-extra.initrd\n\
                 options root={} cast.fstx={}\n",
                support::ROOT_LOCATOR,
                state
            );
            assert_eq!(
                generated_at(&plan, &format!("loader/entries/head-6.13-{state}.conf")),
                expected.as_bytes()
            );
            let paths = plan
                .outputs()
                .iter()
                .map(|output| output.relative_path().to_str().unwrap())
                .collect::<Vec<_>>();
            let entry_612 = format!("loader/entries/head-6.12-{state}.conf");
            let entry_613 = format!("loader/entries/head-6.13-{state}.conf");
            assert_eq!(
                paths,
                [
                    "EFI/head/6.12/vmlinuz",
                    "EFI/head/6.13/10-base.initrd",
                    "EFI/head/6.13/20-extra.initrd",
                    "EFI/head/6.13/vmlinuz",
                    entry_612.as_str(),
                    entry_613.as_str(),
                    "loader/loader.conf",
                    "EFI/Boot/BOOTX64.EFI",
                    "EFI/systemd/systemd-bootx64.efi",
                ]
            );
        }
    );
}

#[test]
fn zero_initrd_entry_retains_blank_line_and_final_newline() {
    let deadline = support::future_deadline();
    with_render_inputs!(support::simple_fixture(), deadline, |fixture, inputs| {
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let topology = topology::alias_topology();
        let (plan, _) = fixture_plan(rendered, &topology);
        let state = i32::from(fixture.head.id);
        let expected = format!(
            "title Render Head (6.12)\nlinux /EFI/head/6.12/vmlinuz\n\noptions root={} cast.fstx={}\n",
            support::ROOT_LOCATOR,
            state
        );
        assert_eq!(
            generated_at(&plan, &format!("loader/entries/head-6.12-{state}.conf")),
            expected.as_bytes()
        );
    });
}

#[test]
fn entry_payload_and_loader_paths_match_exact_bls_shapes() {
    let deadline = support::future_deadline();
    with_render_inputs!(support::simple_fixture(), deadline, |fixture, inputs| {
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let topology = topology::alias_topology();
        let (plan, _) = fixture_plan(rendered, &topology);
        let state = i32::from(fixture.head.id);
        let paths = plan
            .outputs()
            .iter()
            .map(|output| output.relative_path().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), 5);
        assert!(paths.contains(&PathBuf::from("EFI/head/6.12/vmlinuz")));
        assert!(paths.contains(&PathBuf::from(format!("loader/entries/head-6.12-{state}.conf"))));
        assert!(paths.contains(&PathBuf::from("loader/loader.conf")));
        assert!(paths.contains(&PathBuf::from("EFI/Boot/BOOTX64.EFI")));
        assert!(paths.contains(&PathBuf::from("EFI/systemd/systemd-bootx64.efi")));
    });
}
