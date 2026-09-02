#include <libintl.h>
#include <locale.h>
#include <stdio.h>
#include <string.h>

#define DOMAIN "cast-gettext-localization-fixture"
#define SOURCE_MESSAGE "Hello from Cast"

int main(int argc, char **argv)
{
    const char *translated;

    if (argc != 4) {
        fputs("usage: consumer LOCALE LOCALE_ROOT EXPECTED\n", stderr);
        return 64;
    }
    if (setlocale(LC_ALL, argv[1]) == NULL) {
        fprintf(stderr, "locale unavailable: %s\n", argv[1]);
        return 65;
    }
    if (bindtextdomain(DOMAIN, argv[2]) == NULL ||
        bind_textdomain_codeset(DOMAIN, "UTF-8") == NULL ||
        textdomain(DOMAIN) == NULL) {
        fputs("gettext domain setup failed\n", stderr);
        return 66;
    }

    translated = gettext(SOURCE_MESSAGE);
    if (strcmp(translated, SOURCE_MESSAGE) == 0) {
        fputs("untranslated gettext fallback rejected\n", stderr);
        return 67;
    }
    if (strcmp(translated, argv[3]) != 0) {
        fprintf(stderr, "unexpected translation: %s\n", translated);
        return 68;
    }

    puts(translated);
    return 0;
}
