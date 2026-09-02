
#include "config.h"

#include <stdio.h>

int main(void)
{
    return printf(
               "cast autotools fixture: autoreconf build=%s host=%s\n",
               CAST_AUTOTOOLS_BUILD_ALIAS,
               CAST_AUTOTOOLS_HOST_ALIAS
           ) < 0;
}
