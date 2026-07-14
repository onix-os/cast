// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

#include <stdio.h>
#include <string.h>

#include "config.h"

static int check_config(const char *path) {
    FILE *config = fopen(path, "r");
    if (config == NULL) {
        perror(path);
        return 1;
    }
    char line[128];
    const int has_content = fgets(line, sizeof(line), config) != NULL;
    const int close_failed = fclose(config) != 0;
    return !has_content || close_failed;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
        printf("cast daemon fixture %s (%s)\n", CAST_DAEMON_VERSION, CAST_DAEMON_DEFAULT_CONFIG);
        return 0;
    }
    if (argc == 3 && strcmp(argv[1], "--check-config") == 0) {
        return check_config(argv[2]);
    }
    fprintf(stderr, "usage: %s --self-test | --check-config PATH\n", argv[0]);
    return 2;
}
