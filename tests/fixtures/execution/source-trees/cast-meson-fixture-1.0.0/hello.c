
#include <stdio.h>
#include <string.h>
#include <zlib.h>

int main(void)
{
    static const Bytef payload[] = "cast meson fixture declarative payload";
    Bytef compressed[128];
    Bytef restored[sizeof(payload)];
    uLongf compressed_size = sizeof(compressed);
    uLongf restored_size = sizeof(restored);

    if (compress2(compressed, &compressed_size, payload, sizeof(payload), Z_BEST_COMPRESSION) != Z_OK) {
        return 1;
    }
    if (uncompress(restored, &restored_size, compressed, compressed_size) != Z_OK) {
        return 1;
    }
    if (restored_size != sizeof(payload) || memcmp(restored, payload, sizeof(payload)) != 0) {
        return 1;
    }

    return puts("cast meson fixture: pkgconfig zlib round-trip verified") == EOF;
}
