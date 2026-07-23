#include <cast-header-only/vector.h>

#ifndef CAST_HEADER_ONLY_VECTOR_MAGIC
#error "the staged fixture header was not included"
#endif

_Static_assert(CAST_HEADER_ONLY_VECTOR_MAGIC == 0x48445231, "fixture header identity drifted");
_Static_assert(CAST_HEADER_ONLY_VECTOR_ADD(20, 22) == 42, "fixture header API is unusable");

int main(void)
{
    return 0;
}
