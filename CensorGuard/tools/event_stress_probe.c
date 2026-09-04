#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

static int parse_positive(const char *value, unsigned long long maximum,
                          unsigned long long *parsed)
{
    char *end = NULL;
    errno = 0;
    unsigned long long result = strtoull(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || result == 0 || result > maximum)
        return -1;
    *parsed = result;
    return 0;
}

static int connect_events(const char *path, int receive_buffer)
{
    if (strlen(path) >= sizeof(((struct sockaddr_un *)0)->sun_path)) {
        fprintf(stderr, "socket path is too long\n");
        return -1;
    }
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) {
        perror("socket");
        return -1;
    }
    if (receive_buffer > 0 &&
        setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &receive_buffer,
                   sizeof(receive_buffer)) != 0) {
        perror("setsockopt(SO_RCVBUF)");
        close(fd);
        return -1;
    }
    struct sockaddr_un address = {
        .sun_family = AF_UNIX,
    };
    memcpy(address.sun_path, path, strlen(path) + 1);
    if (connect(fd, (const struct sockaddr *)&address, sizeof(address)) != 0) {
        perror("connect");
        close(fd);
        return -1;
    }
    return fd;
}

static int generate_events(const char *path, unsigned long long count)
{
    for (unsigned long long index = 0; index < count; index++) {
        int fd = open(path, O_RDONLY | O_CLOEXEC);
        if (fd < 0) {
            perror("open");
            return 3;
        }
        if (close(fd) != 0) {
            perror("close");
            return 3;
        }
    }
    printf("generated=%llu\n", count);
    return 0;
}

static int slow_subscriber(const char *path, unsigned long long seconds)
{
    int fd = connect_events(path, 1024);
    if (fd < 0)
        return 3;
    puts("connected");
    fflush(stdout);
    while (seconds > 0)
        seconds = sleep((unsigned int)seconds);
    close(fd);
    return 0;
}

static int drain_subscriber(const char *path)
{
    int fd = connect_events(path, 0);
    if (fd < 0)
        return 3;
    FILE *stream = fdopen(fd, "r");
    if (stream == NULL) {
        perror("fdopen");
        close(fd);
        return 3;
    }
    puts("connected");
    fflush(stdout);
    char *line = NULL;
    size_t capacity = 0;
    unsigned long long events = 0;
    unsigned long long denied = 0;
    while (getline(&line, &capacity, stream) >= 0) {
        events++;
        if (strstr(line, "\"allowed\":false") != NULL)
            denied++;
    }
    free(line);
    fclose(stream);
    printf("events=%llu denied=%llu\n", events, denied);
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 4 && strcmp(argv[1], "generate") == 0) {
        unsigned long long count = 0;
        if (parse_positive(argv[3], 10000000, &count) != 0)
            return 2;
        return generate_events(argv[2], count);
    }
    if (argc == 4 && strcmp(argv[1], "slow") == 0) {
        unsigned long long seconds = 0;
        if (parse_positive(argv[3], 300, &seconds) != 0)
            return 2;
        return slow_subscriber(argv[2], seconds);
    }
    if (argc == 3 && strcmp(argv[1], "drain") == 0)
        return drain_subscriber(argv[2]);
    fprintf(stderr,
            "usage: %s generate FILE COUNT|slow SOCKET SECONDS|drain SOCKET\n",
            argv[0]);
    return 2;
}
