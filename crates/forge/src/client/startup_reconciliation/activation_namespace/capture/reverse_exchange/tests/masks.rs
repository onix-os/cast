use super::*;

#[test]
fn reverse_exchange_parent_mask_allows_only_mtime_and_ctime() {
    let original = witness(10);
    let expected = ExchangeStableInode::from(original);

    let mut timestamps = original;
    timestamps.modified_seconds += 1;
    timestamps.modified_nanoseconds += 1;
    timestamps.changed_seconds += 1;
    timestamps.changed_nanoseconds += 1;
    assert_eq!(ExchangeStableInode::from(timestamps), expected);

    let mut changes = Vec::new();
    let mut changed = original;
    changed.device += 1;
    changes.push(changed);
    let mut changed = original;
    changed.inode += 1;
    changes.push(changed);
    let mut changed = original;
    changed.mode ^= 0o100;
    changes.push(changed);
    let mut changed = original;
    changed.owner += 1;
    changes.push(changed);
    let mut changed = original;
    changed.group += 1;
    changes.push(changed);
    let mut changed = original;
    changed.links += 1;
    changes.push(changed);
    let mut changed = original;
    changed.length += 1;
    changes.push(changed);

    for changed in changes {
        assert_ne!(ExchangeStableInode::from(changed), expected);
    }
}

#[test]
fn reverse_exchange_moved_usr_mask_allows_only_ctime() {
    let original = witness(100);
    let expected = MovedUsrInode::from(original);

    let mut ctime = original;
    ctime.changed_seconds += 1;
    ctime.changed_nanoseconds += 1;
    assert_eq!(MovedUsrInode::from(ctime), expected);

    let mut mtime = original;
    mtime.modified_seconds += 1;
    assert_ne!(MovedUsrInode::from(mtime), expected);
    let mut mtime = original;
    mtime.modified_nanoseconds += 1;
    assert_ne!(MovedUsrInode::from(mtime), expected);

    let mut stable_changes = Vec::new();
    let mut changed = original;
    changed.device += 1;
    stable_changes.push(changed);
    let mut changed = original;
    changed.inode += 1;
    stable_changes.push(changed);
    let mut changed = original;
    changed.mode ^= 0o100;
    stable_changes.push(changed);
    let mut changed = original;
    changed.owner += 1;
    stable_changes.push(changed);
    let mut changed = original;
    changed.group += 1;
    stable_changes.push(changed);
    let mut changed = original;
    changed.links += 1;
    stable_changes.push(changed);
    let mut changed = original;
    changed.length += 1;
    stable_changes.push(changed);

    for changed in stable_changes {
        assert_ne!(MovedUsrInode::from(changed), expected);
    }
}
