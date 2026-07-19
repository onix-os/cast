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
                    format!("initrd /{}", checksum_payload_path("head", "A-first.initrd", b"a")),
                    format!("initrd /{}", checksum_payload_path("head", "m-middle.initrd", b"m")),
                    format!("initrd /{}", checksum_payload_path("head", "z-last.initrd", b"z")),
                ]
            );
        }
    );
}

#[test]
fn identical_payload_bytes_and_leaf_reuse_one_path_across_versions_and_bindings() {
    let deadline = support::future_deadline();
    let head = same_version_with_initrd("shared.initrd", b"shared");
    let history = support::StateSpec::one_kernel("6.11")
        .with_kernel(support::KernelSpec::new("6.14").with_initrd("shared.initrd", b"shared"));
    let fixture = support::RenderFixture::new(head, vec![history]);
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
        let shared_path = PathBuf::from(checksum_payload_path("head", "shared.initrd", b"shared"));
        assert_eq!(payloads.iter().filter(|path| *path == &shared_path).count(), 1);
        assert!(!shared_path.to_string_lossy().contains("6.13"));
        assert!(!shared_path.to_string_lossy().contains("6.14"));

        let head_state = i32::from(fixture.head.id);
        let history_state = i32::from(fixture.histories[0].id);
        for entry_path in [
            format!("loader/entries/head-6.13-{head_state}.conf"),
            format!("loader/entries/head-6.14-{history_state}.conf"),
        ] {
            let entry = std::str::from_utf8(generated_at(&plan, &entry_path)).unwrap();
            assert!(entry.contains(&format!("initrd /{}", shared_path.display())));
        }
    });
}

#[test]
fn same_namespace_version_and_leaf_with_different_bytes_use_distinct_paths() {
    let deadline = support::future_deadline();
    let head = same_version_with_initrd("shared.initrd", b"head");
    let history = same_version_with_initrd("shared.initrd", b"past");
    let fixture = support::RenderFixture::new(head, vec![history]);
    remove_history_schema(&fixture);
    with_render_inputs!(fixture, deadline, |fixture, inputs| {
        let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
        let topology = topology::alias_topology();
        let (plan, _) = fixture_plan(rendered, &topology);
        let head_path = checksum_payload_path("head", "shared.initrd", b"head");
        let history_path = checksum_payload_path("head", "shared.initrd", b"past");
        assert_ne!(head_path, history_path);
        for path in [&head_path, &history_path] {
            assert!(
                plan.outputs()
                    .iter()
                    .any(|output| output.relative_path() == Path::new(path))
            );
        }

        let head_state = i32::from(fixture.head.id);
        let history_state = i32::from(fixture.histories[0].id);
        let head_entry = std::str::from_utf8(generated_at(
            &plan,
            &format!("loader/entries/head-6.13-{head_state}.conf"),
        ))
        .unwrap();
        let history_entry = std::str::from_utf8(generated_at(
            &plan,
            &format!("loader/entries/head-6.13-{history_state}.conf"),
        ))
        .unwrap();
        assert!(head_entry.contains(&format!("initrd /{head_path}")));
        assert!(history_entry.contains(&format!("initrd /{history_path}")));
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
