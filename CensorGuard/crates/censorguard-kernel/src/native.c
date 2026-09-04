#include <bpf/bpf.h>
#include <bpf/libbpf.h>
#include <errno.h>
#include <limits.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

struct as_inner_model {
    char outer_name[64];
    enum bpf_map_type type;
    unsigned int key_size;
    unsigned int value_size;
    unsigned int max_entries;
    unsigned int map_flags;
};

struct as_kernel {
    struct bpf_object *object;
    struct bpf_link **links;
    size_t link_count;
    struct as_inner_model inner_models[16];
    size_t inner_model_count;
};

struct as_event_reader {
    struct ring_buffer *ring;
    int drop_map_fd;
    struct {
        size_t size;
        unsigned char data[512];
    } samples[256];
    size_t head;
    size_t count;
    unsigned long dropped;
};

static int copy_event(void *context, void *data, size_t size)
{
    struct as_event_reader *reader = context;
    if (size > sizeof(reader->samples[0].data) || reader->count == 256) {
        reader->dropped++;
        return 0;
    }
    size_t tail = (reader->head + reader->count) % 256;
    reader->samples[tail].size = size;
    memcpy(reader->samples[tail].data, data, size);
    reader->count++;
    return 0;
}

static void set_error(char *buffer, size_t size, const char *format, ...)
{
    if (buffer == NULL || size == 0)
        return;
    va_list args;
    va_start(args, format);
    (void)vsnprintf(buffer, size, format, args);
    va_end(args);
}

static bool required(const char *name, const char *const *programs, size_t count)
{
    for (size_t index = 0; index < count; index++) {
        if (strcmp(name, programs[index]) == 0)
            return true;
    }
    return false;
}

static void cleanup(struct as_kernel *kernel)
{
    if (kernel == NULL)
        return;
    for (size_t index = 0; index < kernel->link_count; index++)
        bpf_link__destroy(kernel->links[index]);
    free(kernel->links);
    bpf_object__close(kernel->object);
    free(kernel);
}

struct as_kernel *as_kernel_load(const char *path,
                                 const char *const *programs,
                                 size_t count,
                                 char *error,
                                 size_t error_size)
{
    struct as_kernel *kernel = calloc(1, sizeof(*kernel));
    if (kernel == NULL) {
        set_error(error, error_size, "allocate kernel session: %s", strerror(errno));
        return NULL;
    }

    kernel->object = bpf_object__open_file(path, NULL);
    if (kernel->object == NULL) {
        set_error(error, error_size, "open BPF object %s: %s", path, strerror(errno));
        cleanup(kernel);
        return NULL;
    }

    struct bpf_map *outer = NULL;
    bpf_object__for_each_map(outer, kernel->object) {
        struct bpf_map *inner = bpf_map__inner_map(outer);
        if (inner == NULL)
            continue;
        if (kernel->inner_model_count >= 16) {
            set_error(error, error_size, "too many map-in-map templates");
            cleanup(kernel);
            return NULL;
        }
        struct as_inner_model *model =
            &kernel->inner_models[kernel->inner_model_count++];
        (void)snprintf(model->outer_name, sizeof(model->outer_name), "%s",
                       bpf_map__name(outer));
        model->type = bpf_map__type(inner);
        model->key_size = bpf_map__key_size(inner);
        model->value_size = bpf_map__value_size(inner);
        model->max_entries = bpf_map__max_entries(inner);
        model->map_flags = bpf_map__map_flags(inner);
    }

    struct bpf_program *program = NULL;
    bpf_object__for_each_program(program, kernel->object) {
        int result = bpf_program__set_autoload(
            program, required(bpf_program__name(program), programs, count));
        if (result != 0) {
            set_error(error, error_size, "set autoload for %s: %s",
                      bpf_program__name(program), strerror(-result));
            cleanup(kernel);
            return NULL;
        }
    }

    for (size_t index = 0; index < count; index++) {
        if (bpf_object__find_program_by_name(kernel->object, programs[index]) == NULL) {
            set_error(error, error_size, "BPF object is missing required program %s",
                      programs[index]);
            cleanup(kernel);
            return NULL;
        }
    }

    int result = bpf_object__load(kernel->object);
    if (result != 0) {
        set_error(error, error_size, "load BPF object: %s", strerror(-result));
        cleanup(kernel);
        return NULL;
    }

    kernel->links = calloc(count, sizeof(*kernel->links));
    if (kernel->links == NULL) {
        set_error(error, error_size, "allocate BPF links: %s", strerror(errno));
        cleanup(kernel);
        return NULL;
    }

    for (size_t index = 0; index < count; index++) {
        program = bpf_object__find_program_by_name(kernel->object, programs[index]);
        const char *section = bpf_program__section_name(program);
        struct bpf_link *link = NULL;
        if (strncmp(section, "lsm/", 4) == 0 || strncmp(section, "lsm.s/", 6) == 0)
            link = bpf_program__attach_lsm(program);
        else
            link = bpf_program__attach(program);

        long attach_error = libbpf_get_error(link);
        if (attach_error != 0) {
            set_error(error, error_size, "attach %s (%s): %s", programs[index], section,
                      strerror((int)-attach_error));
            cleanup(kernel);
            return NULL;
        }
        kernel->links[kernel->link_count++] = link;
    }
    return kernel;
}

void as_kernel_free(struct as_kernel *kernel)
{
    cleanup(kernel);
}

int as_kernel_map_fd(const struct as_kernel *kernel, const char *name)
{
    if (kernel == NULL || kernel->object == NULL) {
        errno = EINVAL;
        return -EINVAL;
    }
    return bpf_object__find_map_fd_by_name(kernel->object, name);
}

int as_kernel_update_map(const struct as_kernel *kernel,
                         const char *name,
                         const void *key,
                         size_t key_size,
                         const void *value,
                         size_t value_size,
                         char *error,
                         size_t error_size)
{
    struct bpf_map *map = bpf_object__find_map_by_name(kernel->object, name);
    if (map == NULL) {
        set_error(error, error_size, "map %s does not exist", name);
        return -ENOENT;
    }
    if (bpf_map__key_size(map) != key_size || bpf_map__value_size(map) != value_size) {
        set_error(error, error_size,
                  "map %s ABI mismatch: key %u/%zu value %u/%zu", name,
                  bpf_map__key_size(map), key_size, bpf_map__value_size(map), value_size);
        return -EINVAL;
    }
    int result = bpf_map_update_elem(bpf_map__fd(map), key, value, BPF_ANY);
    if (result != 0) {
        int saved = errno;
        set_error(error, error_size, "update map %s: %s", name, strerror(saved));
        return -saved;
    }
    return 0;
}

int as_kernel_lookup_map(const struct as_kernel *kernel,
                         const char *name,
                         const void *key,
                         size_t key_size,
                         void *value,
                         size_t value_size,
                         char *error,
                         size_t error_size)
{
    struct bpf_map *map = bpf_object__find_map_by_name(kernel->object, name);
    if (map == NULL) {
        set_error(error, error_size, "map %s does not exist", name);
        return -ENOENT;
    }
    if (bpf_map__key_size(map) != key_size || bpf_map__value_size(map) != value_size) {
        set_error(error, error_size,
                  "map %s ABI mismatch: key %u/%zu value %u/%zu", name,
                  bpf_map__key_size(map), key_size, bpf_map__value_size(map), value_size);
        return -EINVAL;
    }
    if (bpf_map_lookup_elem(bpf_map__fd(map), key, value) != 0) {
        int saved = errno;
        set_error(error, error_size, "lookup map %s: %s", name, strerror(saved));
        return -saved;
    }
    return 0;
}

int as_kernel_replace_inner(const struct as_kernel *kernel,
                            const char *outer_name,
                            unsigned int slot,
                            const void *keys,
                            const void *values,
                            size_t count,
                            size_t key_size,
                            size_t value_size,
                            char *error,
                            size_t error_size)
{
    struct bpf_map *outer = bpf_object__find_map_by_name(kernel->object, outer_name);
    if (outer == NULL) {
        set_error(error, error_size, "outer map %s does not exist", outer_name);
        return -ENOENT;
    }
    const struct as_inner_model *model = NULL;
    for (size_t index = 0; index < kernel->inner_model_count; index++) {
        if (strcmp(kernel->inner_models[index].outer_name, outer_name) == 0) {
            model = &kernel->inner_models[index];
            break;
        }
    }
    if (model == NULL) {
        set_error(error, error_size, "no captured inner template for %s", outer_name);
        return -EINVAL;
    }

    if (model->key_size != key_size || model->value_size != value_size) {
        set_error(error, error_size,
                  "inner map %s ABI mismatch: key %u/%zu value %u/%zu", outer_name,
                  model->key_size, key_size, model->value_size, value_size);
        return -EINVAL;
    }
    if (count > model->max_entries) {
        set_error(error, error_size, "inner map %s has %zu entries; maximum is %u",
                  outer_name, count, model->max_entries);
        return -E2BIG;
    }

    struct bpf_map_create_opts options = {
        .sz = sizeof(options),
        .map_flags = model->map_flags,
    };
    int inner_fd = bpf_map_create(model->type, NULL,
                                  (unsigned int)key_size, (unsigned int)value_size,
                                  model->max_entries, &options);
    if (inner_fd < 0) {
        int saved = errno;
        set_error(error, error_size, "create inner map for %s: %s", outer_name,
                  strerror(saved));
        return -saved;
    }

    for (size_t index = 0; index < count; index++) {
        const char *key = (const char *)keys + index * key_size;
        const char *value = (const char *)values + index * value_size;
        if (bpf_map_update_elem(inner_fd, key, value, BPF_ANY) != 0) {
            int saved = errno;
            set_error(error, error_size, "fill inner map %s entry %zu: %s", outer_name,
                      index, strerror(saved));
            close(inner_fd);
            return -saved;
        }
    }

    int result = bpf_map_update_elem(bpf_map__fd(outer), &slot, &inner_fd, BPF_ANY);
    if (result != 0) {
        int saved = errno;
        set_error(error, error_size, "replace outer map %s slot %u: %s", outer_name,
                  slot, strerror(saved));
        close(inner_fd);
        return -saved;
    }
    close(inner_fd);
    return 0;
}

int as_kernel_clone_rule_slot(const struct as_kernel *kernel,
                              unsigned int source_slot,
                              unsigned int destination_slot,
                              char *error,
                              size_t error_size)
{
    const char *outer_names[] = {
        "dom_file_str", "dom_file_ino", "dom_dir_ino", "dom_cmd",
        "dom_cmd_ino", "dom_arg", "dom_net", "dom_net_port",
        "dom_net6", "dom_net6_port",
    };
    for (size_t index = 0; index < sizeof(outer_names) / sizeof(outer_names[0]); index++) {
        struct bpf_map *outer =
            bpf_object__find_map_by_name(kernel->object, outer_names[index]);
        if (outer == NULL) {
            set_error(error, error_size, "outer map %s does not exist", outer_names[index]);
            return -ENOENT;
        }
        unsigned int map_id = 0;
        if (bpf_map_lookup_elem(bpf_map__fd(outer), &source_slot, &map_id) != 0) {
            int saved = errno;
            set_error(error, error_size, "lookup %s slot %u: %s", outer_names[index],
                      source_slot, strerror(saved));
            return -saved;
        }
        int inner_fd = bpf_map_get_fd_by_id(map_id);
        if (inner_fd < 0) {
            int saved = errno;
            set_error(error, error_size, "open %s inner id %u: %s", outer_names[index],
                      map_id, strerror(saved));
            return -saved;
        }
        if (bpf_map_update_elem(bpf_map__fd(outer), &destination_slot, &inner_fd, BPF_ANY) != 0) {
            int saved = errno;
            set_error(error, error_size, "clone %s slot %u -> %u: %s", outer_names[index],
                      source_slot, destination_slot, strerror(saved));
            close(inner_fd);
            return -saved;
        }
        close(inner_fd);
    }
    return 0;
}

int as_kernel_delete_map(const struct as_kernel *kernel,
                         const char *name,
                         const void *key,
                         size_t key_size,
                         char *error,
                         size_t error_size)
{
    struct bpf_map *map = bpf_object__find_map_by_name(kernel->object, name);
    if (map == NULL) {
        set_error(error, error_size, "map %s does not exist", name);
        return -ENOENT;
    }
    if (bpf_map__key_size(map) != key_size) {
        set_error(error, error_size, "map %s key ABI mismatch: %u/%zu", name,
                  bpf_map__key_size(map), key_size);
        return -EINVAL;
    }
    if (bpf_map_delete_elem(bpf_map__fd(map), key) != 0) {
        int saved = errno;
        if (saved == ENOENT)
            return 0;
        set_error(error, error_size, "delete map %s: %s", name, strerror(saved));
        return -saved;
    }
    return 0;
}

long as_kernel_map_count(const struct as_kernel *kernel,
                         const char *name,
                         char *error,
                         size_t error_size)
{
    struct bpf_map *map = bpf_object__find_map_by_name(kernel->object, name);
    if (map == NULL) {
        set_error(error, error_size, "map %s does not exist", name);
        return -ENOENT;
    }
    size_t key_size = bpf_map__key_size(map);
    void *current = calloc(1, key_size);
    void *next = calloc(1, key_size);
    if (current == NULL || next == NULL) {
        free(current);
        free(next);
        set_error(error, error_size, "allocate map iterator: %s", strerror(errno));
        return -ENOMEM;
    }

    const void *previous = NULL;
    long count = 0;
    while (bpf_map_get_next_key(bpf_map__fd(map), previous, next) == 0) {
        count++;
        void *swap = current;
        current = next;
        next = swap;
        previous = current;
    }
    int saved = errno;
    free(current);
    free(next);
    if (saved != ENOENT) {
        set_error(error, error_size, "iterate map %s: %s", name, strerror(saved));
        return -saved;
    }
    return count;
}

long as_kernel_list_scope(const struct as_kernel *kernel,
                           const char *name,
                           unsigned long long scope_id,
                           unsigned int *pids,
                           size_t capacity,
                           char *error,
                           size_t error_size)
{
    struct bpf_map *map = bpf_object__find_map_by_name(kernel->object, name);
    if (map == NULL) {
        set_error(error, error_size, "%s map does not exist", name);
        return -ENOENT;
    }
    size_t key_size = bpf_map__key_size(map);
    size_t value_size = bpf_map__value_size(map);
    if (key_size != sizeof(*pids) || value_size < sizeof(scope_id)) {
        set_error(error, error_size,
                  "%s ABI mismatch: key %zu value %zu", name, key_size, value_size);
        return -EINVAL;
    }
    void *current = calloc(1, key_size);
    void *next = calloc(1, key_size);
    void *value = calloc(1, value_size);
    if (current == NULL || next == NULL || value == NULL) {
        free(current);
        free(next);
        free(value);
        set_error(error, error_size, "allocate scope PID iterator: %s", strerror(errno));
        return -ENOMEM;
    }
    const void *previous = NULL;
    size_t count = 0;
    while (bpf_map_get_next_key(bpf_map__fd(map), previous, next) == 0) {
        if (bpf_map_lookup_elem(bpf_map__fd(map), next, value) == 0 &&
            *(const unsigned long long *)value == scope_id) {
            if (count >= capacity) {
                free(current);
                free(next);
                free(value);
                set_error(error, error_size,
                          "scope %llu has more than %zu tracked PIDs", scope_id, capacity);
                return -E2BIG;
            }
            pids[count++] = *(const unsigned int *)next;
        }
        void *swap = current;
        current = next;
        next = swap;
        previous = current;
    }
    int saved = errno;
    free(current);
    free(next);
    free(value);
    if (saved != ENOENT) {
        set_error(error, error_size, "iterate %s: %s", name, strerror(saved));
        return -saved;
    }
    return (long)count;
}

struct as_event_reader *as_event_reader_new(const struct as_kernel *kernel,
                                             char *error,
                                             size_t error_size)
{
    int map_fd = bpf_object__find_map_fd_by_name(kernel->object, "events");
    if (map_fd < 0) {
        set_error(error, error_size, "events map does not exist");
        return NULL;
    }
    int drop_map_fd = bpf_object__find_map_fd_by_name(kernel->object, "event_drops");
    if (drop_map_fd < 0) {
        set_error(error, error_size, "event_drops map does not exist");
        return NULL;
    }
    struct as_event_reader *reader = calloc(1, sizeof(*reader));
    if (reader == NULL) {
        set_error(error, error_size, "allocate event reader: %s", strerror(errno));
        return NULL;
    }
    reader->ring = ring_buffer__new(map_fd, copy_event, reader, NULL);
    long ring_error = libbpf_get_error(reader->ring);
    if (ring_error != 0) {
        set_error(error, error_size, "open events ring buffer: %s",
                  strerror((int)-ring_error));
        free(reader);
        return NULL;
    }
    reader->drop_map_fd = drop_map_fd;
    return reader;
}

void as_event_reader_free(struct as_event_reader *reader)
{
    if (reader == NULL)
        return;
    ring_buffer__free(reader->ring);
    free(reader);
}

int as_event_reader_next(struct as_event_reader *reader,
                         void *output,
                         size_t output_size,
                         int timeout_ms,
                         char *error,
                         size_t error_size)
{
    if (reader == NULL || output == NULL)
        return -EINVAL;
    if (reader->count == 0) {
        int result = ring_buffer__poll(reader->ring, timeout_ms);
        if (result < 0) {
            set_error(error, error_size, "poll events ring buffer: %s", strerror(-result));
            return result;
        }
    }
    if (reader->count == 0)
        return 0;
    size_t sample_size = reader->samples[reader->head].size;
    if (sample_size != output_size) {
        set_error(error, error_size, "event ABI mismatch: kernel %zu / Rust %zu",
                  sample_size, output_size);
        return -EMSGSIZE;
    }
    memcpy(output, reader->samples[reader->head].data, sample_size);
    reader->head = (reader->head + 1) % 256;
    reader->count--;
    return 1;
}

unsigned long as_event_reader_dropped(const struct as_event_reader *reader)
{
    return reader == NULL ? 0 : reader->dropped;
}

unsigned long as_event_reader_kernel_dropped(const struct as_event_reader *reader)
{
    if (reader == NULL)
        return 0;
    unsigned int key = 0;
    unsigned long long kernel_dropped = 0;
    if (bpf_map_lookup_elem(reader->drop_map_fd, &key, &kernel_dropped) != 0)
        return 0;
    return kernel_dropped > ULONG_MAX ? ULONG_MAX : (unsigned long)kernel_dropped;
}
