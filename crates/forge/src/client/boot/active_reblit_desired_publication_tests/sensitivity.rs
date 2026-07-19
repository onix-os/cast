use super::*;

fn fingerprint(
    outputs: &[DesiredActiveReblitBootPublication],
    layout: ActiveReblitBootDestinationLayout,
) -> ActiveReblitDesiredPublicationFingerprint {
    prepare_fixture(outputs, layout).fingerprint()
}

#[test]
fn canonical_order_is_deterministic_independent_of_input_order() {
    let forward = fixture_outputs();
    let mut reverse = fixture_outputs();
    reverse.reverse();

    let forward = prepare_fixture(&forward, ActiveReblitBootDestinationLayout::BootAliasesEsp);
    let reverse = prepare_fixture(&reverse, ActiveReblitBootDestinationLayout::BootAliasesEsp);
    assert_eq!(forward.outputs(), reverse.outputs());
    assert_eq!(forward.fingerprint(), reverse.fingerprint());
    assert_eq!(forward.path_bytes(), reverse.path_bytes());
    assert_eq!(forward.canonical_bytes(), reverse.canonical_bytes());
    assert_eq!(forward.work(), reverse.work());
}

#[test]
fn every_canonical_scalar_changes_the_fingerprint() {
    let baseline = fingerprint(&fixture_outputs(), ActiveReblitBootDestinationLayout::BootAliasesEsp);

    assert_ne!(
        baseline,
        fingerprint(&fixture_outputs(), ActiveReblitBootDestinationLayout::DistinctXbootldr)
    );

    let mut changed = fixture_outputs();
    changed[0].root = ActiveReblitBootDestinationRoot::Boot;
    assert_ne!(
        baseline,
        fingerprint(&changed, ActiveReblitBootDestinationLayout::BootAliasesEsp)
    );

    let mut changed = fixture_outputs();
    changed[0].phase = ActiveReblitBootPublicationPhase::LoaderControl;
    assert_ne!(
        baseline,
        fingerprint(&changed, ActiveReblitBootDestinationLayout::BootAliasesEsp)
    );

    let mut changed = fixture_outputs();
    changed[0].role = ActiveReblitBootPublicationRole::SystemdBootloader;
    assert_ne!(
        baseline,
        fingerprint(&changed, ActiveReblitBootDestinationLayout::BootAliasesEsp)
    );

    let mut changed = fixture_outputs();
    changed[0].relative_path = PathBuf::from("EFI/Boot/OTHERX64.EFI");
    assert_ne!(
        baseline,
        fingerprint(&changed, ActiveReblitBootDestinationLayout::BootAliasesEsp)
    );

    let mut changed = fixture_outputs();
    changed[0].mode ^= 0o100;
    assert_ne!(
        baseline,
        fingerprint(&changed, ActiveReblitBootDestinationLayout::BootAliasesEsp)
    );

    let mut changed = fixture_outputs();
    changed[0].checksum ^= 1;
    assert_ne!(
        baseline,
        fingerprint(&changed, ActiveReblitBootDestinationLayout::BootAliasesEsp)
    );

    let mut changed = fixture_outputs();
    changed[0].length += 1;
    assert_ne!(
        baseline,
        fingerprint(&changed, ActiveReblitBootDestinationLayout::BootAliasesEsp)
    );

    let mut changed = fixture_outputs();
    changed[0].content_identity = BootContentIdentity::hash(b"third-bytes");
    assert_ne!(
        baseline,
        fingerprint(&changed, ActiveReblitBootDestinationLayout::BootAliasesEsp)
    );

    let mut removed = fixture_outputs();
    removed.pop();
    assert_ne!(
        baseline,
        fingerprint(&removed, ActiveReblitBootDestinationLayout::BootAliasesEsp)
    );

    let mut added = fixture_outputs();
    added.push(DesiredActiveReblitBootPublication {
        root: ActiveReblitBootDestinationRoot::Boot,
        phase: ActiveReblitBootPublicationPhase::LoaderControl,
        role: ActiveReblitBootPublicationRole::LoaderControl,
        relative_path: PathBuf::from("loader/loader.conf"),
        mode: ACTIVE_REBLIT_BOOT_OUTPUT_MODE,
        checksum: 0x55,
        length: 7,
        content_identity: BootContentIdentity::hash(b"default"),
    });
    assert_ne!(
        baseline,
        fingerprint(&added, ActiveReblitBootDestinationLayout::BootAliasesEsp)
    );
}

#[test]
fn canonical_v1_fingerprint_is_pinned() {
    let inventory = prepare_fixture(&fixture_outputs(), ActiveReblitBootDestinationLayout::BootAliasesEsp);
    assert_eq!(
        hex::encode(inventory.fingerprint().as_bytes()),
        "2833e9d5da0a1e90d2b35e62d88bae722292f5a9689466326dd86ecc792077dd"
    );
    assert_eq!(inventory.canonical_bytes(), 872);
}
