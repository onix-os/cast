#include <dlfcn.h>
#include <stdio.h>
#include <string.h>

typedef const char *(*plugin_identity_fn)(void);

static const char default_plugin[] =
    "/usr/lib/cast/plugins/cast-plugin-output.so";
static const char expected_identity[] =
    "cast plugin output fixture: loaded explicitly";

static int report_loader_error(const char *operation, const char *error)
{
    if (error == NULL) {
        error = "unknown dynamic-loader error";
    }
    (void)fprintf(stderr, "%s: %s\n", operation, error);
    return 1;
}

int main(int argc, char **argv)
{
    const char *plugin_path = default_plugin;
    plugin_identity_fn identity;
    const char *error;
    const char *message;
    void *handle;
    void *symbol;
    int status;

    if (argc == 3 && strcmp(argv[1], "--plugin") == 0) {
        plugin_path = argv[2];
    } else if (argc != 1) {
        return 64;
    }

    handle = dlopen(plugin_path, RTLD_NOW | RTLD_LOCAL);
    if (handle == NULL) {
        return report_loader_error("dlopen", dlerror());
    }

    dlerror();
    symbol = dlsym(handle, "cast_plugin_output_identity");
    error = dlerror();
    if (error != NULL || symbol == NULL) {
        status = report_loader_error("dlsym", error);
        (void)dlclose(handle);
        return status;
    }

    _Static_assert(sizeof(identity) == sizeof(symbol),
                   "function and object pointers must have equal size");
    memcpy(&identity, &symbol, sizeof(identity));
    message = identity();
    if (message == NULL || strcmp(message, expected_identity) != 0) {
        (void)dlclose(handle);
        return 1;
    }
    if (puts(message) == EOF) {
        (void)dlclose(handle);
        return 1;
    }

    return dlclose(handle) == 0
        ? 0
        : report_loader_error("dlclose", dlerror());
}
