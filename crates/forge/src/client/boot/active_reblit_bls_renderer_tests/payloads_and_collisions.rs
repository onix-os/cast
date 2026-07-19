use super::*;

fn same_version_with_initrd(name: &str, bytes: &[u8]) -> support::StateSpec {
    support::StateSpec::one_kernel("6.12").with_kernel(support::KernelSpec::new("6.13").with_initrd(name, bytes))
}

fn remove_history_schema(fixture: &support::RenderFixture) {
    std::fs::remove_file(
        fixture
            .installation
            .root_path(fixture.histories[0].id.to_string())
            .join("usr/lib/os-release"),
    )
    .unwrap();
}

#[test]
fn initrd_basenames_are_preserved_and_sorted_ascii_case_insensitively() {
    let deadline = support::future_deadline();
    let spec = support::StateSpec::one_kernel("6.12").with_kernel(
        support::KernelSpec::new("6.13")
            .with_initrd("z-last.initrd", b"z".as_slice())
            .with_initrd("A-first.initrd", b"a".as_slice())
            .with_initrd("m-middle.initrd", b"m".as_slice()),
    );
    with_render_inputs!(
        support::RenderFixture::new(spec, Vec::new()),
        deadline,
        |fixture, inputs| {
            let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
            let topology = topology::alias_topology();
            let (plan, _) = fixture_plan(rendered, &topology);
            let state = i32::from(fixture.head.id);
            let entry =
                std::str::from_utf8(generated_at(&plan, &format!("loader/entries/head-6.13-{state}.conf"))).unwrap();
            let initrds = entry
                .lines()
                .filter(|line| line.starts_with("initrd "))
                .collect::<Vec<_>>();
            assert_eq!(
                initrds,
                [
                    "initrd /EFI/head/6.13/A-first.initrd",
                    "initrd /EFI/head/6.13/m-middle.initrd",
                    "initrd /EFI/head/6.13/z-last.initrd",
                ]
            );
        }
    );
}

#[test]
fn identical_payload_path_digest_and_length_deduplicate_across_binding_indices() {
    let deadline = support::future_deadline();
    let spec = same_version_with_initrd("shared.initrd", b"shared");
    let fixture = support::RenderFixture::new(spec.clone(), vec![spec]);
    remove_history_schema(&fixture);
    with_render_inputs!(fixture, deadline, |fixture, inputs| {
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let topology = topology::alias_topology();
        let (plan, _) = fixture_plan(rendered, &topology);
        let payloads = plan
            .outputs()
            .iter()
            .filter(|output| output.role() == ActiveReblitBootPublicationRole::Payload)
            .map(|output| output.relative_path().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(payloads.len(), 3);
        assert_eq!(
            payloads.iter().filter(|path| path.ends_with("shared.initrd")).count(),
            1
        );
    });
}

#[test]
fn same_payload_path_with_different_content_is_rejected() {
    let deadline = support::future_deadline();
    let head = same_version_with_initrd("shared.initrd", b"head");
    let history = same_version_with_initrd("shared.initrd", b"history");
    let fixture = support::RenderFixture::new(head, vec![history]);
    remove_history_schema(&fixture);
    with_render_inputs!(fixture, deadline, |_fixture, inputs| {
        assert!(matches!(
            RenderedActiveReblitBlsRequests::render(&inputs),
            Err(ActiveReblitBlsRendererError::PayloadCollision { .. })
        ));
    });
}

#[test]
fn case_insensitive_payload_alias_is_rejected_even_when_content_matches() {
    let deadline = support::future_deadline();
    let head = same_version_with_initrd("Shared.initrd", b"same");
    let history = same_version_with_initrd("shared.initrd", b"same");
    let fixture = support::RenderFixture::new(head, vec![history]);
    remove_history_schema(&fixture);
    with_render_inputs!(fixture, deadline, |_fixture, inputs| {
        assert!(matches!(
            RenderedActiveReblitBlsRequests::render(&inputs),
            Err(ActiveReblitBlsRendererError::PayloadCaseCollision { .. })
        ));
    });
}
