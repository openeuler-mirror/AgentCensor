#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

static int denied_result(const char *operation, long result)
{
    if (result >= 0) {
        printf("%s unexpectedly succeeded\n", operation);
        return 0;
    }
    int saved = errno;
    printf("%s errno=%d %s\n", operation, saved, strerror(saved));
    return saved == EPERM ? 1 : 3;
}

static int stop_for_attach(void);

static int probe_read(int fd)
{
    if (stop_for_attach() != 0)
        return 4;
    unsigned char byte = 0;
    errno = 0;
    return denied_result("fd_read", read(fd, &byte, sizeof(byte)));
}

static int probe_write(int fd)
{
    if (stop_for_attach() != 0)
        return 4;
    static const char payload[] = "blocked-write";
    errno = 0;
    return denied_result("fd_write", write(fd, payload, sizeof(payload) - 1));
}

static int stop_for_attach(void)
{
    if (raise(SIGSTOP) == 0)
        return 0;
    perror("raise(SIGSTOP)");
    return -1;
}

static int probe_ftruncate(int fd)
{
    if (stop_for_attach() != 0)
        return 4;
    errno = 0;
    return denied_result("ftruncate", ftruncate(fd, 0));
}

static int probe_mmap(int fd, size_t length)
{
    if (stop_for_attach() != 0)
        return 4;
    errno = 0;
    void *mapping = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (mapping == MAP_FAILED)
        return denied_result("mmap_write", -1);
    (void)munmap(mapping, length);
    return denied_result("mmap_write", 0);
}

static int probe_mprotect(int fd, size_t length)
{
    void *mapping = mmap(NULL, length, PROT_READ, MAP_SHARED, fd, 0);
    if (mapping == MAP_FAILED) {
        perror("initial mmap");
        return 4;
    }
    if (stop_for_attach() != 0) {
        (void)munmap(mapping, length);
        return 4;
    }
    errno = 0;
    int result = mprotect(mapping, length, PROT_READ | PROT_WRITE);
    int saved = errno;
    (void)munmap(mapping, length);
    errno = saved;
    return denied_result("mprotect", result);
}

int main(int argc, char **argv)
{
    if (argc != 3) {
        fprintf(stderr, "usage: %s read|write|ftruncate|mmap|mprotect FILE\n", argv[0]);
        return 2;
    }

    int fd = open(argv[2], O_RDWR | O_CLOEXEC);
    if (fd < 0) {
        perror("open");
        return 4;
    }
    struct stat metadata;
    if (fstat(fd, &metadata) != 0) {
        perror("fstat");
        close(fd);
        return 4;
    }
    long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0 || metadata.st_size < page_size) {
        fprintf(stderr, "file must contain at least one memory page\n");
        close(fd);
        return 4;
    }

    int result;
    if (strcmp(argv[1], "read") == 0)
        result = probe_read(fd);
    else if (strcmp(argv[1], "write") == 0)
        result = probe_write(fd);
    else if (strcmp(argv[1], "ftruncate") == 0)
        result = probe_ftruncate(fd);
    else if (strcmp(argv[1], "mmap") == 0)
        result = probe_mmap(fd, (size_t)page_size);
    else if (strcmp(argv[1], "mprotect") == 0)
        result = probe_mprotect(fd, (size_t)page_size);
    else {
        fprintf(stderr, "unknown operation: %s\n", argv[1]);
        result = 2;
    }
    close(fd);
    return result;
}
