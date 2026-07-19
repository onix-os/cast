use super::super::BoundActiveReblitMountedBootTopology;
use super::support::{AliasFixture, DISK_SEQUENCE, MOUNT_POINT, PARTUUID};

#[test]
fn alias_fixture_retains_exact_descriptor_backed_scalar_facts() {
    let fixture = AliasFixture::stable().unwrap();
    let feed = fixture.feed();
    let prepared = fixture.prepare().unwrap();

    assert_eq!(feed.read_count(), 4, "bootstrap and three later passes each read once");

    let view = prepared.revalidate(&fixture.installation).unwrap();
    assert_eq!(feed.read_count(), 7, "one revalidation adds exactly three snapshots");
    let BoundActiveReblitMountedBootTopology::BootAliasesEsp { esp } = view.topology() else {
        panic!("alias intent retained a distinct target shape");
    };
    assert_eq!(esp.selector, MOUNT_POINT);
    assert_eq!(esp.partuuid, PARTUUID);
    assert_eq!(
        (esp.destination.raw_device, esp.destination.inode),
        fixture.destination_identity()
    );
    assert_eq!(esp.mount_id, fixture.destination_identity().1);
    assert_eq!((esp.device_major(), esp.device_minor()), fixture.device());
    assert_eq!(esp.partition_number.get(), 1);
    assert_eq!(esp.partition_uuid.as_str(), PARTUUID);
    assert_eq!(esp.disk_sequence.map(|sequence| sequence.get()), Some(DISK_SEQUENCE));
    fixture.assert_outside_unchanged();
}

#[test]
fn repeated_revalidation_keeps_the_bootstrap_topology_exact() {
    let fixture = AliasFixture::stable().unwrap();
    let prepared = fixture.prepare().unwrap();
    let first_view = prepared.revalidate(&fixture.installation).unwrap();
    let first = first_view.topology();
    let second_view = prepared.revalidate(&fixture.installation).unwrap();
    let second = second_view.topology();

    assert_eq!(first, second);
    fixture.assert_outside_unchanged();
}
