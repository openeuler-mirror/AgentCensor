#include <arpa/inet.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    const char *target = "127.0.0.1";
    unsigned long port = 18080;
    if (argc == 3) {
        char *end = NULL;
        errno = 0;
        port = strtoul(argv[2], &end, 10);
        if (errno != 0 || end == argv[2] || *end != '\0' || port == 0 || port > 65535) {
            fprintf(stderr, "invalid port: %s\n", argv[2]);
            return 4;
        }
        target = argv[1];
    } else if (argc != 1) {
        fprintf(stderr, "usage: %s [IP PORT]\n", argv[0]);
        return 4;
    }

    struct sockaddr_storage storage = {0};
    socklen_t address_len = 0;
    int family = AF_UNSPEC;
    struct sockaddr_in *address4 = (struct sockaddr_in *)&storage;
    struct sockaddr_in6 *address6 = (struct sockaddr_in6 *)&storage;
    if (inet_pton(AF_INET, target, &address4->sin_addr) == 1) {
        family = AF_INET;
        address4->sin_family = AF_INET;
        address4->sin_port = htons((unsigned short)port);
        address_len = sizeof(*address4);
    } else if (inet_pton(AF_INET6, target, &address6->sin6_addr) == 1) {
        family = AF_INET6;
        address6->sin6_family = AF_INET6;
        address6->sin6_port = htons((unsigned short)port);
        address_len = sizeof(*address6);
    } else {
        return 3;
    }

    int fd = socket(family, SOCK_STREAM, 0);
    if (fd < 0)
        return 2;
    if (connect(fd, (const struct sockaddr *)&storage, address_len) == 0) {
        close(fd);
        puts("errno=0");
        return 0;
    }
    int saved = errno;
    close(fd);
    printf("errno=%d %s\n", saved, strerror(saved));
    return saved == EPERM ? 1 : 0;
}
