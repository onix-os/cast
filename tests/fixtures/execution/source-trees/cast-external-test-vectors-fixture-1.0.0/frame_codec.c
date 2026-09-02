#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum {
    maximum_corpus_bytes = 4096,
    required_vector_count = 3,
};

static int verify_codec(void)
{
    unsigned int value;

    for (value = 0; value <= 255; value++) {
        char encoded[3];

        if (snprintf(encoded, sizeof(encoded), "%02x", value) != 2) {
            return 1;
        }
        if (strtoul(encoded, NULL, 16) != value) {
            return 1;
        }
    }
    return 0;
}

static int verify_vectors(const char *path)
{
    char corpus[maximum_corpus_bytes + 1];
    char *cursor;
    FILE *stream;
    int close_status;
    int read_status;
    size_t size;
    unsigned int vectors = 0;

    errno = 0;
    stream = fopen(path, "rb");
    if (stream == NULL) {
        fprintf(stderr, "cannot open external vector corpus: %s\n", strerror(errno));
        return 1;
    }
    size = fread(corpus, 1, maximum_corpus_bytes + 1, stream);
    read_status = ferror(stream);
    close_status = fclose(stream);
    if (read_status != 0 || close_status != 0) {
        fputs("cannot read external vector corpus\n", stderr);
        return 1;
    }
    if (size == 0 || size > maximum_corpus_bytes) {
        fputs("external vector corpus is empty or oversized\n", stderr);
        return 1;
    }
    corpus[size] = '\0';
    if (strstr(corpus, "\"schema\":1") == NULL) {
        fputs("external vector corpus has the wrong schema\n", stderr);
        return 1;
    }

    cursor = corpus;
    while ((cursor = strstr(cursor, "{\"plain\":")) != NULL) {
        char encoded[3] = {0};
        char expected[3];
        int consumed = 0;
        unsigned int plain;

        if (sscanf(cursor, "{\"plain\":%u,\"encoded\":\"%2[0-9a-f]\"}%n", &plain, encoded, &consumed) != 2
            || consumed <= 0 || plain > 255) {
            fputs("external vector corpus contains a malformed vector\n", stderr);
            return 1;
        }
        if (snprintf(expected, sizeof(expected), "%02x", plain) != 2 || strcmp(encoded, expected) != 0) {
            fputs("external vector corpus disagrees with the codec\n", stderr);
            return 1;
        }
        vectors++;
        cursor += consumed;
    }
    if (vectors != required_vector_count) {
        fprintf(stderr, "external vector corpus contains %u vectors instead of %u\n", vectors, required_vector_count);
        return 1;
    }
    puts("cast external test vectors fixture: 3 independently locked vectors verified");
    return 0;
}

int main(int argc, char **argv)
{
    if (verify_codec() != 0) {
        fputs("frame codec self-test failed\n", stderr);
        return 1;
    }
    if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
        puts("cast external test vectors fixture: codec self-test passed");
        return 0;
    }
    if (argc == 3 && strcmp(argv[1], "--vectors") == 0) {
        return verify_vectors(argv[2]);
    }
    fputs("usage: cast-external-test-vectors-fixture --self-test | --vectors FILE\n", stderr);
    return 64;
}
