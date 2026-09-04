#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <unistd.h>

extern char **environ;

static void usage(const char *program)
{
    fprintf(stderr, "usage: %s absolute|relative|empty PATH [ARG ...]\n", program);
}

int main(int argc, char **argv)
{
    if (argc < 3) {
        usage(argv[0]);
        return 2;
    }

    const char *mode = argv[1];
    const char *path = argv[2];
    char **target_argv = calloc((size_t)argc - 1, sizeof(*target_argv));
    if (target_argv == NULL) {
        perror("calloc");
        return 1;
    }
    target_argv[0] = (char *)path;
    for (int index = 3; index < argc; index++)
        target_argv[index - 2] = argv[index];

    int dirfd = AT_FDCWD;
    int flags = 0;
    const char *filename = path;
    char *path_copy = NULL;

    if (strcmp(mode, "relative") == 0) {
        path_copy = strdup(path);
        if (path_copy == NULL) {
            perror("strdup");
            free(target_argv);
            return 1;
        }
        char *slash = strrchr(path_copy, '/');
        if (slash == NULL || slash == path_copy || slash[1] == '\0') {
            fprintf(stderr, "relative mode requires an absolute non-root path\n");
            free(path_copy);
            free(target_argv);
            return 2;
        }
        *slash = '\0';
        filename = slash + 1;
        dirfd = open(path_copy, O_PATH | O_DIRECTORY | O_CLOEXEC);
    } else if (strcmp(mode, "empty") == 0) {
        dirfd = open(path, O_PATH | O_CLOEXEC);
        filename = "";
        flags = AT_EMPTY_PATH;
    } else if (strcmp(mode, "absolute") != 0) {
        usage(argv[0]);
        free(target_argv);
        return 2;
    }

    if (dirfd < 0 && dirfd != AT_FDCWD) {
        perror("open execveat target");
        free(path_copy);
        free(target_argv);
        return 1;
    }

    (void)syscall(SYS_execveat, dirfd, filename, target_argv, environ, flags);
    int saved = errno;
    fprintf(stderr, "execveat %s %s: %s\n", mode, path, strerror(saved));
    if (dirfd != AT_FDCWD)
        close(dirfd);
    free(path_copy);
    free(target_argv);
    return saved == EPERM ? 77 : 1;
}
