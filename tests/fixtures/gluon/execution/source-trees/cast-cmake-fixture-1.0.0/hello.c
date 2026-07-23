
#include <stdio.h>
#include <string.h>
#include <zlib.h>

int main(void) {
    static const Bytef input[] =
        "cast cmake fixture: deterministic zlib payload";
    Bytef compressed[128];
    Bytef restored[sizeof(input)];
    uLongf compressed_len = sizeof(compressed);
    uLongf restored_len = sizeof(restored);

    if (compress2(compressed, &compressed_len, input, sizeof(input),
                  Z_BEST_COMPRESSION) != Z_OK) {
        return 1;
    }
    if (uncompress(restored, &restored_len, compressed, compressed_len) != Z_OK) {
        return 2;
    }
    if (restored_len != sizeof(input) ||
        memcmp(restored, input, sizeof(input)) != 0) {
        return 3;
    }

    return puts("cast cmake fixture: zlib round-trip verified") == EOF ? 4 : 0;
}
