use std::fs::File;

use super::super::mountinfo::read_mountinfo_bounded;

#[test]
fn live_thread_self_snapshot_is_parser_compatible_without_granting_authority() {
    // This is only a compatibility fixture for the Linux grammar. Opening a
    // public procfs path here does not authenticate procfs or produce a
    // retained capability suitable for production topology decisions.
    let mut mountinfo = File::open("/proc/thread-self/mountinfo").unwrap();
    let parsed = read_mountinfo_bounded(&mut mountinfo).unwrap();

    assert!(!parsed.entries().is_empty());
    assert!(parsed.entries().iter().any(|entry| entry.mount_point() == b"/"));
}
