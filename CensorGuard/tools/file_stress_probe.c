#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

struct stress_state {
    int protected_fd;
    int allowed_fd;
    const char *protected_path;
    uint64_t iterations;
    atomic_bool start;
    atomic_uint_fast64_t denied_ok;
    atomic_uint_fast64_t allowed_ok;
    atomic_uint_fast64_t errors;
};

static uint64_t parse_count(const char *name, const char *value, uint64_t maximum)
{
    char *end = NULL;
    errno = 0;
    unsigned long long parsed = strtoull(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || parsed == 0 || parsed > maximum) {
        fprintf(stderr, "invalid %s: %s\n", name, value);
        exit(2);
    }
    return (uint64_t)parsed;
}

static void record_denied(struct stress_state *state, long result, int saved_errno)
{
    if (result < 0 && saved_errno == EPERM)
        atomic_fetch_add_explicit(&state->denied_ok, 1, memory_order_relaxed);
    else
        atomic_fetch_add_explicit(&state->errors, 1, memory_order_relaxed);
}

static void record_allowed(struct stress_state *state, long result)
{
    if (result == 1)
        atomic_fetch_add_explicit(&state->allowed_ok, 1, memory_order_relaxed);
    else
        atomic_fetch_add_explicit(&state->errors, 1, memory_order_relaxed);
}

static void *worker_main(void *opaque)
{
    struct stress_state *state = opaque;
    while (!atomic_load_explicit(&state->start, memory_order_acquire))
        sched_yield();

    for (uint64_t iteration = 0; iteration < state->iterations; iteration++) {
        unsigned char byte = 0;
        errno = 0;
        ssize_t result = pread(state->protected_fd, &byte, sizeof(byte), 0);
        record_denied(state, result, errno);

        errno = 0;
        result = pwrite(state->protected_fd, "x", 1, 0);
        record_denied(state, result, errno);

        errno = 0;
        int fd = open(state->protected_path, O_RDONLY | O_CLOEXEC);
        int saved_errno = errno;
        if (fd >= 0)
            (void)close(fd);
        record_denied(state, fd, saved_errno);

        errno = 0;
        fd = open(state->protected_path, O_WRONLY | O_CLOEXEC);
        saved_errno = errno;
        if (fd >= 0)
            (void)close(fd);
        record_denied(state, fd, saved_errno);

        result = pread(state->allowed_fd, &byte, sizeof(byte), 0);
        record_allowed(state, result);
        result = pwrite(state->allowed_fd, "a", 1, 0);
        record_allowed(state, result);
    }
    return NULL;
}

static uint64_t elapsed_ns(const struct timespec *start, const struct timespec *end)
{
    return (uint64_t)(end->tv_sec - start->tv_sec) * 1000000000ULL
        + (uint64_t)(end->tv_nsec - start->tv_nsec);
}

int main(int argc, char **argv)
{
    if (argc != 5) {
        fprintf(stderr, "usage: %s PROTECTED ALLOWED THREADS ITERATIONS\n", argv[0]);
        return 2;
    }
    const uint64_t thread_count = parse_count("threads", argv[3], 128);
    const uint64_t iterations = parse_count("iterations", argv[4], 1000000);

    struct stress_state state = {
        .protected_fd = open(argv[1], O_RDWR | O_CLOEXEC),
        .allowed_fd = open(argv[2], O_RDWR | O_CLOEXEC),
        .protected_path = argv[1],
        .iterations = iterations,
        .start = ATOMIC_VAR_INIT(false),
        .denied_ok = ATOMIC_VAR_INIT(0),
        .allowed_ok = ATOMIC_VAR_INIT(0),
        .errors = ATOMIC_VAR_INIT(0),
    };
    if (state.protected_fd < 0 || state.allowed_fd < 0) {
        perror("open stress files");
        return 1;
    }
    if (raise(SIGSTOP) != 0) {
        perror("raise(SIGSTOP)");
        return 1;
    }

    pthread_t *threads = calloc((size_t)thread_count, sizeof(*threads));
    if (threads == NULL) {
        perror("calloc threads");
        return 1;
    }
    uint64_t created = 0;
    for (; created < thread_count; created++) {
        int error = pthread_create(&threads[created], NULL, worker_main, &state);
        if (error != 0) {
            fprintf(stderr, "pthread_create: %s\n", strerror(error));
            atomic_store_explicit(&state.start, true, memory_order_release);
            break;
        }
    }
    struct timespec start = {};
    struct timespec end = {};
    (void)clock_gettime(CLOCK_MONOTONIC, &start);
    atomic_store_explicit(&state.start, true, memory_order_release);
    bool join_failed = created != thread_count;
    for (uint64_t index = 0; index < created; index++) {
        int error = pthread_join(threads[index], NULL);
        if (error != 0) {
            fprintf(stderr, "pthread_join: %s\n", strerror(error));
            join_failed = true;
        }
    }
    (void)clock_gettime(CLOCK_MONOTONIC, &end);

    const uint64_t denied = atomic_load_explicit(&state.denied_ok, memory_order_relaxed);
    const uint64_t allowed = atomic_load_explicit(&state.allowed_ok, memory_order_relaxed);
    const uint64_t errors = atomic_load_explicit(&state.errors, memory_order_relaxed);
    const uint64_t expected_denied = thread_count * iterations * 4;
    const uint64_t expected_allowed = thread_count * iterations * 2;
    const uint64_t operations = expected_denied + expected_allowed;
    const uint64_t duration = elapsed_ns(&start, &end);
    const double ops_per_second = duration == 0
        ? 0.0
        : (double)operations * 1000000000.0 / (double)duration;
    printf("threads=%" PRIu64 " iterations=%" PRIu64
           " denied_ok=%" PRIu64 " allowed_ok=%" PRIu64
           " errors=%" PRIu64 " elapsed_ms=%.3f ops_per_sec=%.0f\n",
           thread_count, iterations, denied, allowed, errors,
           (double)duration / 1000000.0, ops_per_second);

    free(threads);
    (void)close(state.protected_fd);
    (void)close(state.allowed_fd);
    return join_failed || errors != 0 || denied != expected_denied
        || allowed != expected_allowed;
}
