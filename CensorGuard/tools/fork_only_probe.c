#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void pause_millis(long millis)
{
    struct timespec delay = {
        .tv_sec = millis / 1000,
        .tv_nsec = (millis % 1000) * 1000000,
    };
    while (nanosleep(&delay, &delay) != 0 && errno == EINTR) {
    }
}

static int write_pid(const char *path, pid_t pid)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0)
        return -1;
    char buffer[32];
    int length = snprintf(buffer, sizeof(buffer), "%ld\n", (long)pid);
    ssize_t written = write(fd, buffer, (size_t)length);
    int saved = errno;
    close(fd);
    errno = saved;
    return written == length ? 0 : -1;
}

int main(int argc, char **argv)
{
    if (argc != 4) {
        fprintf(stderr, "usage: %s GO_FILE CHILD_PID_FILE CHILD_SECONDS\n", argv[0]);
        return 2;
    }
    char *end = NULL;
    long child_seconds = strtol(argv[3], &end, 10);
    if (end == argv[3] || *end != '\0' || child_seconds <= 0 || child_seconds > 60)
        return 2;

    while (access(argv[1], F_OK) != 0) {
        if (errno != ENOENT) {
            perror("access");
            return 3;
        }
        pause_millis(10);
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return 4;
    }
    if (child == 0) {
        if (write_pid(argv[2], getpid()) != 0)
            _exit(5);
        for (long second = 0; second < child_seconds; second++)
            sleep(1);
        _exit(0);
    }
    int status = 0;
    if (waitpid(child, &status, 0) < 0) {
        perror("waitpid");
        return 6;
    }
    return WIFEXITED(status) ? WEXITSTATUS(status) : 7;
}
