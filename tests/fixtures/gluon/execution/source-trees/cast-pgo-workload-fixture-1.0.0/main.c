#include <stdint.h>
#include <stdio.h>
#include <string.h>

static const char final_identity[] =
    "cast PGO workload fixture: profile-use binary executed";
static const char training_identity[] =
    "cast PGO workload fixture: instrumented training completed";

static uint32_t score_text(const char *text)
{
    uint32_t score = 0;

    while (*text != '\0') {
        const unsigned char byte = (unsigned char)*text++;

        if (byte >= (unsigned char)'a' && byte <= (unsigned char)'z') {
            score += (uint32_t)(byte - (unsigned char)'a') + 1U;
        } else if (byte >= (unsigned char)'0' && byte <= (unsigned char)'9') {
            score += (uint32_t)(byte - (unsigned char)'0') * 3U;
        } else {
            score += 1U;
        }
    }

    return score;
}

static int train(const char *corpus)
{
    volatile uint32_t observed = 0;

    for (uint32_t round = 0; round < 16384U; ++round) {
        const uint32_t sample = score_text(corpus);

        if ((round & 7U) == 0U) {
            observed ^= sample + round;
        } else if ((round & 1U) == 0U) {
            observed += sample ^ round;
        } else {
            observed += sample + (round & 31U);
        }
    }

    if (observed == 0U) {
        return 1;
    }
    return puts(training_identity) == EOF;
}

int main(int argc, char **argv)
{
    if (argc == 3 && strcmp(argv[1], "--train") == 0) {
        return train(argv[2]);
    }
    if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
        if (score_text("abc") != 6U || score_text("profile") != 81U) {
            return 1;
        }
        return puts(final_identity) == EOF;
    }
    if (argc != 1) {
        return 64;
    }
    return puts(final_identity) == EOF;
}
