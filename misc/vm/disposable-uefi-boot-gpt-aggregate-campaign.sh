#!/bin/sh

set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
LC_ALL=C
export PATH LC_ALL

base=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)/disposable-uefi-boot-storage-campaign.sh
[ -f "$base" ] && [ ! -L "$base" ] || {
    printf '%s\n' 'disposable VM GPT aggregate wrapper is missing its guarded base campaign' >&2
    exit 1
}

expect_target_disk=0
live_system_disk=/dev/"vda"
for argument do
    if [ "$expect_target_disk" -eq 1 ]; then
        case "$argument" in
            "$live_system_disk" | "$live_system_disk"[0-9]*)
                printf '%s\n' 'the GPT aggregate wrapper refuses every primary live-system disk target' >&2
                exit 2
                ;;
        esac
        expect_target_disk=0
        continue
    fi
    case "$argument" in
        --campaign-profile)
            printf '%s\n' 'the GPT aggregate wrapper owns its campaign profile' >&2
            exit 2
            ;;
        --target-disk) expect_target_disk=1 ;;
    esac
done
[ "$expect_target_disk" -eq 0 ] || {
    printf '%s\n' 'the GPT aggregate wrapper requires a target-disk value' >&2
    exit 2
}

exec "$base" "$@" --campaign-profile gpt-receipt-bound-aggregate-v1
