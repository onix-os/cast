
#include <stdio.h>

#include "config.h"

int main(void) {
    return puts(CAST_AUTOTOOLS_OPTIONS_MESSAGE) == EOF;
}
