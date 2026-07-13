#!/bin/bash
# SPDX-FileCopyrightText: 2025 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

installkernel() {
    return 0
}

check() {
    if [[ -x $systemdutildir/systemd ]] && [[ -x /usr/lib/cast/cast-fstx.sh ]]; then
       return 255
    fi

    return 1
}

depends() {
    return 0
}

install() {
    dracut_install /usr/lib/cast/cast-fstx.sh
    dracut_install /usr/bin/cast

    inst_simple "${systemdsystemunitdir}/cast-fstx.service"
    # Enable systemd type unit(s)
    $SYSTEMCTL -q --root "$initdir" enable cast-fstx.service
}
