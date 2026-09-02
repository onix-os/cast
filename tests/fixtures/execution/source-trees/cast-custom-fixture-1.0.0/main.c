#include <stdio.h>
#include <string.h>

static const char fixture_message[] = "cast custom fixture: compiled and executed";

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--self-test") == 0) {
        return puts(fixture_message) == EOF;
    }
    if (argc != 1) {
        return 64;
    }
    return puts(fixture_message) == EOF;
}
