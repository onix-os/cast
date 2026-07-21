use super::*;

#[test]
fn alias_and_distinct_routes_preserve_global_plan_order() {
    let alias = ActiveReblitBootDestinationLayout::BootAliasesEsp;
    for root in [
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootDestinationRoot::Esp,
    ] {
        assert_eq!(destination_role(alias, root), BootTargetRole::Esp);
    }
    for index in 0..5 {
        assert_eq!(
            domain_plan_position(BootTargetRole::Esp, &[0, 1, 2, 3, 4], index).unwrap(),
            index,
        );
    }

    let distinct = ActiveReblitBootDestinationLayout::DistinctXbootldr;
    let roots = [
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootDestinationRoot::Esp,
        ActiveReblitBootDestinationRoot::Boot,
        ActiveReblitBootDestinationRoot::Esp,
        ActiveReblitBootDestinationRoot::Boot,
    ];
    let esp = [1, 3];
    let xbootldr = [0, 2, 4];
    let mut reconstructed = Vec::new();
    for (plan_index, root) in roots.into_iter().enumerate() {
        let role = destination_role(distinct, root);
        let domain = match role {
            BootTargetRole::Esp => esp.as_slice(),
            BootTargetRole::Xbootldr => xbootldr.as_slice(),
        };
        let position = domain_plan_position(role, domain, plan_index).unwrap();
        assert_eq!(domain[position], plan_index);
        reconstructed.push(plan_index);
    }
    assert_eq!(reconstructed, [0, 1, 2, 3, 4]);
    assert!(matches!(
        domain_plan_position(BootTargetRole::Esp, &esp, 2),
        Err(ActiveReblitBootImmutablePublicationAttemptError::DomainPlanIndexMissing {
            role: BootTargetRole::Esp,
            plan_index: 2,
        }),
    ));
}
