#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

enum { EXPECTED_PATH_CAPACITY = 4096 };
static const char proof_bytes[] = "staged-probe: staged install self-test passed\n";

static int write_proof(const char *proof_path)
{
    FILE *proof = fopen(proof_path, "wb");
    int failed;

    if (proof == NULL) {
        fputs("staged-probe: cannot create staged proof\n", stderr);
        return 1;
    }
    failed = fchmod(fileno(proof), 0644) != 0;
    if (!failed) {
        failed = fwrite(proof_bytes, 1, sizeof(proof_bytes) - 1, proof) != sizeof(proof_bytes) - 1;
    }
    if (!failed) {
        failed = fflush(proof) != 0;
    }
    if (fclose(proof) != 0) {
        failed = 1;
    }
    if (failed) {
        fputs("staged-probe: cannot publish staged proof\n", stderr);
        return 1;
    }
    return 0;
}

static int staged_self_test(const char *invoked_path)
{
    const char *install_root = getenv("CAST_INSTALL_ROOT");
    const char *bindir = getenv("CAST_BINDIR");
    const char *datadir = getenv("CAST_DATADIR");
    char expected_path[EXPECTED_PATH_CAPACITY];
    char proof_path[EXPECTED_PATH_CAPACITY];
    int length;

    if (install_root == NULL || install_root[0] != '/' || bindir == NULL || bindir[0] != '/' ||
        datadir == NULL || datadir[0] != '/') {
        fputs("staged-probe: staged environment is absent\n", stderr);
        return 1;
    }

    length = snprintf(expected_path,
                      sizeof(expected_path),
                      "%s%s/staged-probe",
                      install_root,
                      bindir);
    if (length < 0 || (size_t)length >= sizeof(expected_path)) {
        fputs("staged-probe: staged executable path is too long\n", stderr);
        return 1;
    }
    if (strcmp(invoked_path, expected_path) != 0) {
        fputs("staged-probe: staged executable path mismatch\n", stderr);
        return 1;
    }
    length = snprintf(proof_path,
                      sizeof(proof_path),
                      "%s%s/cast/post-install-smoke-test.proof",
                      install_root,
                      datadir);
    if (length < 0 || (size_t)length >= sizeof(proof_path)) {
        fputs("staged-probe: staged proof path is too long\n", stderr);
        return 1;
    }
    if (write_proof(proof_path) != 0) {
        return 1;
    }

    fputs(proof_bytes, stdout);
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 1) {
        puts("staged-probe: build-tree check passed");
        return 0;
    }
    if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
        return staged_self_test(argv[0]);
    }

    fputs("usage: staged-probe [--self-test]\n", stderr);
    return 64;
}
