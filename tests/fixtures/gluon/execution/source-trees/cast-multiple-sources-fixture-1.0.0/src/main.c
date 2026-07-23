#include <stdio.h>
#include <string.h>

#include "vendor_protocol.h"
#include "protocol-schema.h"

static const char fixture_identity[] =
    "cast multiple sources fixture: archive-main+" CAST_VENDOR_PROTOCOL_ID "+" CAST_RAW_SCHEMA_ID;

int main(int argc, char **argv)
{
    if (argc == 1) {
        return puts(fixture_identity) == EOF;
    }
    if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
        return strcmp(fixture_identity,
                      "cast multiple sources fixture: archive-main+git-protocol-v2+raw-schema-v3") == 0
            && puts(fixture_identity) != EOF
            ? 0
            : 1;
    }

    fputs("usage: cast-multiple-sources-fixture [--self-test]\n", stderr);
    return 64;
}
