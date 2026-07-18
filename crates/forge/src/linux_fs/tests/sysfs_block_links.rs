use std::io;

use super::super::sysfs_block::{
    LinkLimits, normalize_sysfs_dev_block_target, normalize_sysfs_dev_block_target_with_limits_and_work,
    parse_sysfs_subsystem_target, parse_sysfs_subsystem_target_with_limits_and_work,
};

fn limits(max_bytes: usize) -> LinkLimits {
    LinkLimits {
        max_bytes,
        max_components: 64,
        max_component_bytes: max_bytes.min(255),
        max_work: 64 * 1024,
    }
}

fn invalid_data<T: std::fmt::Debug>(result: io::Result<T>) {
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn dev_block_target_normalizes_relative_to_the_fixed_base_as_raw_bytes() {
    let mut target = b"../../devices/pci0000:00/0000:00:14.0/block/sda/".to_vec();
    target.push(0xff);
    let normalized = normalize_sysfs_dev_block_target(&target).unwrap();
    assert_eq!(
        normalized.components().collect::<Vec<_>>(),
        [
            b"devices".as_slice(),
            b"pci0000:00",
            b"0000:00:14.0",
            b"block",
            b"sda",
            b"\xff",
        ]
    );
    assert_eq!(normalized.basename(), b"\xff");
}

#[test]
fn dev_block_target_rejects_escape_dot_empty_and_non_devices_forms() {
    for target in [
        b"".as_slice(),
        b"/devices/block/sda",
        b"../devices/block/sda",
        b"../../../devices/block/sda",
        b"../../class/block/sda",
        b"../../devices",
        b"../../devices/./sda",
        b"../../devices//sda",
        b"../../devices/sda/",
        b"../../devices/sda/../sdb",
        b"../../devices/sda\0evil",
        b"devices/block/sda",
    ] {
        invalid_data(normalize_sysfs_dev_block_target(target));
    }
}

#[test]
fn dev_block_target_bounds_accept_n_and_reject_n_plus_one() {
    let target = b"../../devices/x";
    let exact = limits(target.len());
    normalize_sysfs_dev_block_target_with_limits_and_work(target, exact).unwrap();
    invalid_data(normalize_sysfs_dev_block_target_with_limits_and_work(
        b"../../devices/xy",
        exact,
    ));

    let mut component_limit = limits(64);
    component_limit.max_components = 4;
    normalize_sysfs_dev_block_target_with_limits_and_work(target, component_limit).unwrap();
    invalid_data(normalize_sysfs_dev_block_target_with_limits_and_work(
        b"../../devices/x/y",
        component_limit,
    ));

    let mut component_bytes = limits(64);
    component_bytes.max_component_bytes = 8;
    normalize_sysfs_dev_block_target_with_limits_and_work(b"../../devices/abcdefgh", component_bytes).unwrap();
    invalid_data(normalize_sysfs_dev_block_target_with_limits_and_work(
        b"../../devices/abcdefghi",
        component_bytes,
    ));
}

#[test]
fn dev_block_target_work_bound_accepts_exact_consumption_and_rejects_one_less() {
    let target = b"../../devices/pci/block/sda/sda1";
    let generous = limits(target.len());
    let (_, consumed) = normalize_sysfs_dev_block_target_with_limits_and_work(target, generous).unwrap();

    let mut exact = generous;
    exact.max_work = consumed;
    let (_, exact_consumed) = normalize_sysfs_dev_block_target_with_limits_and_work(target, exact).unwrap();
    assert_eq!(exact_consumed, consumed);

    let mut one_less = generous;
    one_less.max_work = consumed - 1;
    invalid_data(normalize_sysfs_dev_block_target_with_limits_and_work(target, one_less));
}

#[test]
fn subsystem_target_extracts_only_a_validated_final_basename() {
    assert_eq!(
        parse_sysfs_subsystem_target(b"../../../../class/block")
            .unwrap()
            .as_bytes(),
        b"block"
    );
    assert_eq!(
        parse_sysfs_subsystem_target(b"../../bus/nvme-subsystem")
            .unwrap()
            .as_bytes(),
        b"nvme-subsystem"
    );
    assert_eq!(parse_sysfs_subsystem_target(b"block").unwrap().as_bytes(), b"block");
}

#[test]
fn subsystem_target_rejects_ambiguous_or_noncanonical_basenames() {
    for target in [
        b"".as_slice(),
        b"/class/block",
        b"../../class/./block",
        b"../../class//block",
        b"../../class/block/",
        b"../../class/block/../net",
        b"../../..",
        b"../../class/:block",
        b"../../class/\xff",
        b"../../class/block\0evil",
    ] {
        invalid_data(parse_sysfs_subsystem_target(target));
    }
}

#[test]
fn subsystem_bounds_and_work_are_exact() {
    let target = b"../../class/block";
    let generous = limits(target.len());
    let (_, consumed) = parse_sysfs_subsystem_target_with_limits_and_work(target, generous).unwrap();

    let mut exact = generous;
    exact.max_work = consumed;
    let (_, exact_consumed) = parse_sysfs_subsystem_target_with_limits_and_work(target, exact).unwrap();
    assert_eq!(exact_consumed, consumed);

    let mut one_less = generous;
    one_less.max_work = consumed - 1;
    invalid_data(parse_sysfs_subsystem_target_with_limits_and_work(target, one_less));

    let mut component_limit = generous;
    component_limit.max_components = 4;
    parse_sysfs_subsystem_target_with_limits_and_work(target, component_limit).unwrap();
    invalid_data(parse_sysfs_subsystem_target_with_limits_and_work(
        b"../../../class/block",
        component_limit,
    ));
}

#[test]
fn link_parsers_reject_zero_or_incoherent_limits() {
    let mut zero = limits(32);
    zero.max_components = 0;
    assert_eq!(
        normalize_sysfs_dev_block_target_with_limits_and_work(b"../../devices/x", zero)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );

    let mut incoherent = limits(8);
    incoherent.max_component_bytes = 9;
    assert_eq!(
        parse_sysfs_subsystem_target_with_limits_and_work(b"block", incoherent)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}
