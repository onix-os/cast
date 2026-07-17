#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int wait_for_success(pid_t child)
{
    int status;

    while (waitpid(child, &status, 0) == -1) {
        if (errno == EINTR) {
            continue;
        }
        perror("waitpid");
        return 1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "file check exited unsuccessfully\n");
        return 1;
    }
    return 0;
}

int main(int argc, char **argv)
{
    static const char expected[] = "application/x-pie-executable\n";
    char output[128];
    size_t used = 0;
    int descriptors[2];
    pid_t child;

    if (argc != 2) {
        fprintf(stderr, "usage: cast-meson-check-file-input EXECUTABLE\n");
        return 64;
    }
    if (pipe(descriptors) == -1) {
        perror("pipe");
        return 1;
    }

    child = fork();
    if (child == -1) {
        perror("fork");
        (void)close(descriptors[0]);
        (void)close(descriptors[1]);
        return 1;
    }
    if (child == 0) {
        (void)close(descriptors[0]);
        if (dup2(descriptors[1], STDOUT_FILENO) == -1) {
            _exit(126);
        }
        (void)close(descriptors[1]);
        execlp("file", "file", "--brief", "--mime-type", "--", argv[1], (char *)NULL);
        _exit(127);
    }

    (void)close(descriptors[1]);
    while (used < sizeof(output) - 1) {
        ssize_t count = read(descriptors[0], output + used, sizeof(output) - 1 - used);

        if (count == 0) {
            break;
        }
        if (count == -1) {
            if (errno == EINTR) {
                continue;
            }
            perror("read");
            (void)close(descriptors[0]);
            (void)wait_for_success(child);
            return 1;
        }
        used += (size_t)count;
    }
    (void)close(descriptors[0]);
    output[used] = '\0';

    if (wait_for_success(child) != 0) {
        return 1;
    }
    if (strcmp(output, expected) != 0) {
        fprintf(stderr, "file reported unexpected MIME type: %s", output);
        return 1;
    }

    return 0;
}
