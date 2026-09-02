
#include <stdio.h>

#include "libcastsplit.h"

int main(void) {
    return puts(cast_split_message()) == EOF;
}
