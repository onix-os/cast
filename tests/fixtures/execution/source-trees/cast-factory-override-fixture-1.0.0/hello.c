
#include <stdio.h>

#ifndef CAST_FACTORY_VARIANT
#error "CAST_FACTORY_VARIANT must be supplied by the selected CMake builder"
#endif

int main(void) {
    return puts("Stone-native factory override: " CAST_FACTORY_VARIANT) == EOF;
}
