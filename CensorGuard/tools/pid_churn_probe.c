#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

struct child_result {
    pid_t pid;
    int denied;
};

static bool exists(const char *path)
{
    return access(path, F_OK) == 0;
}

static void wait_for_path(const char *path)
{
    while (!exists(path))
        (void)usleep(10000);
}

static int parse_children(const char *value)
{
    char *end = NULL;
    errno = 0;
    long parsed = strtol(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || parsed < 1 || parsed > 2048) {
        fprintf(stderr, "invalid child count: %s\n", value);
        exit(2);
    }
    return (int)parsed;
}

static int write_all(int fd, const void *buffer, size_t size)
{
    const unsigned char *cursor = buffer;
    while (size > 0) {
        ssize_t written = write(fd, cursor, size);
        if (written < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        cursor += (size_t)written;
        size -= (size_t)written;
    }
    return 0;
}

static int read_all(int fd, void *buffer, size_t size)
{
    unsigned char *cursor = buffer;
    while (size > 0) {
        ssize_t received = read(fd, cursor, size);
        if (received == 0)
            return -1;
        if (received < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        cursor += (size_t)received;
        size -= (size_t)received;
    }
    return 0;
}

static void child_main(int result_fd, const char *release_path,
                       const char *protected_path)
{
    int file_fd = open(protected_path, O_RDONLY | O_CLOEXEC);
    int saved = errno;
    if (file_fd >= 0)
        (void)close(file_fd);
    struct child_result result = {
        .pid = getpid(),
        .denied = file_fd < 0 && (saved == EPERM || saved == EACCES),
    };
    if (write_all(result_fd, &result, sizeof(result)) != 0)
        _exit(3);
    (void)close(result_fd);
    wait_for_path(release_path);
    _exit(result.denied ? 0 : 4);
}

int main(int argc, char **argv)
{
    if (argc != 6) {
        fprintf(stderr,
                "usage: %s START READY RELEASE PROTECTED CHILDREN\n",
                argv[0]);
        return 2;
    }
    const int child_count = parse_children(argv[5]);
    int pipe_fds[2];
    if (pipe(pipe_fds) != 0) {
        perror("pipe");
        return 1;
    }

    wait_for_path(argv[1]);
    pid_t *children = calloc((size_t)child_count, sizeof(*children));
    if (children == NULL) {
        perror("calloc");
        return 1;
    }
    for (int index = 0; index < child_count; index++) {
        pid_t pid = fork();
        if (pid < 0) {
            perror("fork");
            for (int prior = 0; prior < index; prior++)
                (void)kill(children[prior], SIGKILL);
            free(children);
            return 1;
        }
        if (pid == 0) {
            (void)close(pipe_fds[0]);
            child_main(pipe_fds[1], argv[3], argv[4]);
        }
        children[index] = pid;
    }
    (void)close(pipe_fds[1]);

    FILE *ready = fopen(argv[2], "w");
    if (ready == NULL) {
        perror("fopen ready");
        return 1;
    }
    bool all_denied = true;
    for (int index = 0; index < child_count; index++) {
        struct child_result result;
        if (read_all(pipe_fds[0], &result, sizeof(result)) != 0) {
            fprintf(stderr, "short child result stream\n");
            return 1;
        }
        if (fprintf(ready, "%d %d\n", result.pid, result.denied) < 0)
            return 1;
        all_denied = all_denied && result.denied != 0;
    }
    if (fclose(ready) != 0) {
        perror("fclose ready");
        return 1;
    }
    (void)close(pipe_fds[0]);

    int exit_status = all_denied ? 0 : 1;
    for (int index = 0; index < child_count; index++) {
        int status = 0;
        while (waitpid(children[index], &status, 0) < 0 && errno == EINTR) {
        }
        if (!WIFEXITED(status) || WEXITSTATUS(status) != 0)
            exit_status = 1;
    }
    free(children);
    return exit_status;
}
