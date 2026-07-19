use super::*;

fn output_signature(plan: &PreparedActiveReblitBootPublicationPlan) -> Vec<(PathBuf, u128, u64, Option<Vec<u8>>)> {
    plan.outputs()
        .iter()
        .map(|output| {
            (
                output.relative_path().to_owned(),
                output.source().digest(),
                output.source().length(),
                output.source().generated_bytes().map(<[u8]>::to_vec),
            )
        })
        .collect()
}

#[test]
fn aliased_and_distinct_topologies_preserve_rendered_bytes_but_change_collision_domains() {
    let deadline = support::future_deadline();
    with_render_inputs!(support::simple_fixture(), deadline, |_fixture, inputs| {
        let alias = topology::alias_topology();
        let distinct = topology::distinct_topology();
        let (alias_plan, _) = fixture_plan(RenderedActiveReblitBlsRequests::render(&inputs).unwrap(), &alias);
        let (distinct_plan, _) = fixture_plan(RenderedActiveReblitBlsRequests::render(&inputs).unwrap(), &distinct);

        assert_eq!(output_signature(&alias_plan), output_signature(&distinct_plan));
        assert!(alias_plan.collision_domains_match(alias.bound()));
        assert!(!alias_plan.collision_domains_match(distinct.bound()));
        assert!(distinct_plan.collision_domains_match(distinct.bound()));
        assert!(!distinct_plan.collision_domains_match(alias.bound()));
    });
}

#[test]
fn bound_plan_retains_exact_inputs_topology_and_sources_without_namespace_mutation() {
    let deadline = support::future_deadline();
    let spec = support::StateSpec::one_kernel("6.12")
        .with_kernel(support::KernelSpec::new("6.13").with_initrd("boot.initrd", b"initrd".as_slice()));
    with_render_inputs!(
        support::RenderFixture::new(spec, Vec::new()),
        deadline,
        |fixture, inputs| {
            let before = support::TreeSnapshot::capture(&fixture.installation.root);
            let topology_fixture = crate::client::active_reblit_mounted_boot_topology::AliasFixture::stable()
                .expect("synthetic topology fixture must be available");
            let topology_before = support::TreeSnapshot::capture(&topology_fixture.installation().root);
            let topology_prepared = topology_fixture
                .prepare_until(deadline)
                .expect("synthetic topology preparation must succeed");
            let topology_view = topology_prepared
                .revalidate_until(topology_fixture.installation(), deadline)
                .expect("synthetic topology revalidation must succeed");
            let rendered = RenderedActiveReblitBlsRequests::render(&inputs).unwrap();
            let plan = rendered
                .into_publication_plan(&topology_view)
                .expect("production topology-aware planning path must succeed");
            assert_eq!(plan.input_deadline(), deadline);
            assert!(plan.collision_domains_still_match());
            let mut sealed = 0usize;
            for output in plan.outputs() {
                match output.sealed_coordinate().unwrap() {
                    Some((_binding_index, digest, length)) => {
                        let asset = output
                            .sealed_asset()
                            .unwrap()
                            .expect("sealed coordinate must resolve to its exact retained asset");
                        assert_eq!(asset.digest(), digest);
                        assert_eq!(asset.length(), length);
                        sealed += 1;
                    }
                    None => assert!(output.sealed_asset().unwrap().is_none()),
                }
            }
            assert_eq!(sealed, 5);
            assert_eq!(before, support::TreeSnapshot::capture(&fixture.installation.root));
            assert_eq!(
                topology_before,
                support::TreeSnapshot::capture(&topology_fixture.installation().root)
            );
            topology_fixture.assert_outside_unchanged();
        }
    );
}
