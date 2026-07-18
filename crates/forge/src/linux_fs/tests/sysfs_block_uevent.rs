use std::io;

use super::super::sysfs_block::{UeventLimits, parse_sysfs_uevent, parse_sysfs_uevent_with_limits_and_work};

fn limits(max_bytes: usize) -> UeventLimits {
    UeventLimits {
        max_bytes,
        max_lines: 8,
        max_line_bytes: max_bytes,
        max_key_bytes: max_bytes.min(64),
        max_work: 64 * 1024,
    }
}

fn invalid_data<T: std::fmt::Debug>(result: io::Result<T>) {
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn uevent_retains_order_unknown_keys_empty_values_and_opaque_value_bytes() {
    let event = parse_sysfs_uevent(b"MAJOR=259\nFUTURE=a=b\nEMPTY=\nOPAQUE=\xff\xfe\n").unwrap();
    assert_eq!(event.fields().len(), 4);
    assert_eq!(event.fields()[0].key(), b"MAJOR");
    assert_eq!(event.fields()[0].value(), b"259");
    assert_eq!(event.fields()[1].key(), b"FUTURE");
    assert_eq!(event.fields()[1].value(), b"a=b");
    assert_eq!(event.value(b"EMPTY"), Some(b"".as_slice()));
    assert_eq!(event.value(b"OPAQUE"), Some(b"\xff\xfe".as_slice()));
    assert_eq!(event.value(b"ABSENT"), None);
}

#[test]
fn uevent_rejects_duplicate_keys_and_noncanonical_line_grammar() {
    for input in [
        b"A=1\nA=2\n".as_slice(),
        b"=value\n",
        b"lower=value\n",
        b"1KEY=value\n",
        b"BAD-KEY=value\n",
        b"KEY\n",
        b"KEY=value\n\n",
        b"KEY=value\0x\n",
        b"KEY=value\r\n",
    ] {
        invalid_data(parse_sysfs_uevent(input));
    }
    assert_eq!(
        parse_sysfs_uevent(b"").unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert_eq!(
        parse_sysfs_uevent(b"KEY=value").unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn uevent_byte_and_line_bounds_accept_n_and_reject_n_plus_one() {
    let one = b"A=x\n";
    parse_sysfs_uevent_with_limits_and_work(one, limits(one.len())).unwrap();
    invalid_data(parse_sysfs_uevent_with_limits_and_work(b"A=xx\n", limits(one.len())));

    let two_lines = b"A=x\nB=y\n";
    let mut line_limits = limits(32);
    line_limits.max_lines = 2;
    parse_sysfs_uevent_with_limits_and_work(two_lines, line_limits).unwrap();
    invalid_data(parse_sysfs_uevent_with_limits_and_work(b"A=x\nB=y\nC=z\n", line_limits));

    let mut per_line = limits(32);
    per_line.max_line_bytes = 4;
    per_line.max_key_bytes = 4;
    parse_sysfs_uevent_with_limits_and_work(b"A=xx\n", per_line).unwrap();
    invalid_data(parse_sysfs_uevent_with_limits_and_work(b"A=xxx\n", per_line));
}

#[test]
fn uevent_key_bound_is_exactly_sixty_four_bytes() {
    let key_64 = vec![b'A'; 64];
    let mut at_limit = key_64.clone();
    at_limit.extend_from_slice(b"=x\n");
    parse_sysfs_uevent(&at_limit).unwrap();

    let mut beyond_limit = key_64;
    beyond_limit.push(b'A');
    beyond_limit.extend_from_slice(b"=x\n");
    invalid_data(parse_sysfs_uevent(&beyond_limit));
}

#[test]
fn uevent_work_bound_accepts_exact_consumption_and_rejects_one_less() {
    let input = b"ALPHA=one\nBETA=two\nFUTURE=three=four\n";
    let generous = limits(input.len());
    let (_, consumed) = parse_sysfs_uevent_with_limits_and_work(input, generous).unwrap();

    let mut exact = generous;
    exact.max_work = consumed;
    let (_, exact_consumed) = parse_sysfs_uevent_with_limits_and_work(input, exact).unwrap();
    assert_eq!(exact_consumed, consumed);

    let mut one_less = generous;
    one_less.max_work = consumed - 1;
    invalid_data(parse_sysfs_uevent_with_limits_and_work(input, one_less));
}

#[test]
fn uevent_rejects_zero_or_incoherent_configured_limits() {
    let mut zero = limits(8);
    zero.max_work = 0;
    assert_eq!(
        parse_sysfs_uevent_with_limits_and_work(b"A=x\n", zero)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );

    let mut incoherent = limits(8);
    incoherent.max_line_bytes = 9;
    assert_eq!(
        parse_sysfs_uevent_with_limits_and_work(b"A=x\n", incoherent)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}
