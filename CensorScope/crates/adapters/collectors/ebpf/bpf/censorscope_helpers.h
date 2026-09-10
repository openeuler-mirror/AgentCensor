#ifndef CENSORSCOPE_HELPERS_H
#define CENSORSCOPE_HELPERS_H

#include <linux/bpf.h>
#include <linux/sched.h>
#include <linux/types.h>

#define SEC(NAME) __attribute__((section(NAME), used))
#define __uint(name, val) int(*name)[val]
#define __type(name, val) val *name
#ifndef __noinline
#define __noinline __attribute__((noinline))
#endif

#define CENSORSCOPE_BPF_FUNC_SEND_SIGNAL 109
#define CENSORSCOPE_BPF_FUNC_PROBE_READ_USER 112
#define CENSORSCOPE_BPF_FUNC_PROBE_READ_KERNEL 113
#define CENSORSCOPE_BPF_FUNC_PROBE_READ_USER_STR 114
#define CENSORSCOPE_BPF_FUNC_PROBE_READ_KERNEL_STR 115
#define CENSORSCOPE_BPF_FUNC_GET_NS_CURRENT_PID_TGID 120
#define CENSORSCOPE_BPF_FUNC_RINGBUF_OUTPUT 130
#define CENSORSCOPE_BPF_FUNC_RINGBUF_RESERVE 131
#define CENSORSCOPE_BPF_FUNC_RINGBUF_SUBMIT 132
#define CENSORSCOPE_BPF_FUNC_RINGBUF_DISCARD 133
#define CENSORSCOPE_BPF_MAP_TYPE_RINGBUF 27


struct task_struct;

static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)
    BPF_FUNC_map_lookup_elem;
static long (*bpf_map_update_elem)(void *map,
                                   const void *key,
                                   const void *value,
                                   __u64 flags) = (void *)
    BPF_FUNC_map_update_elem;
static long (*bpf_map_delete_elem)(void *map, const void *key) = (void *)
    BPF_FUNC_map_delete_elem;
static __u64 (*bpf_get_current_pid_tgid)(void) = (void *)
    BPF_FUNC_get_current_pid_tgid;
static struct task_struct *(*bpf_get_current_task)(void) = (void *)
    BPF_FUNC_get_current_task;
static __u64 (*bpf_ktime_get_ns)(void) = (void *)BPF_FUNC_ktime_get_ns;
static long (*bpf_ringbuf_output)(void *ringbuf,
                                  void *data,
                                  __u64 size,
                                  __u64 flags) = (void *)
    CENSORSCOPE_BPF_FUNC_RINGBUF_OUTPUT;
static void *(*bpf_ringbuf_reserve)(void *ringbuf, __u64 size, __u64 flags) =
    (void *)CENSORSCOPE_BPF_FUNC_RINGBUF_RESERVE;
static void (*bpf_ringbuf_submit)(void *data, __u64 flags) = (void *)
    CENSORSCOPE_BPF_FUNC_RINGBUF_SUBMIT;
static long (*bpf_probe_read_kernel)(void *dst,
                                     __u32 size,
                                     const void *unsafe_ptr) = (void *)
    CENSORSCOPE_BPF_FUNC_PROBE_READ_KERNEL;
static long (*bpf_probe_read_kernel_str)(void *dst,
                                         __u32 size,
                                         const void *unsafe_ptr) = (void *)
    CENSORSCOPE_BPF_FUNC_PROBE_READ_KERNEL_STR;
static long (*bpf_probe_read_user)(void *dst,
                                   __u32 size,
                                   const void *unsafe_ptr) = (void *)
    CENSORSCOPE_BPF_FUNC_PROBE_READ_USER;
static long (*bpf_probe_read_user_str)(void *dst,
                                       __u32 size,
                                       const void *unsafe_ptr) = (void *)
    CENSORSCOPE_BPF_FUNC_PROBE_READ_USER_STR;

/* Keep helper sizes verifier-visible: clang may otherwise widen a clamped
 * 32-bit size back to the original unbounded 64-bit source, so a constant
 * arm plus masked smaller arm is accepted by kernels that drop the
 * source-level range proof. */
static __always_inline long
censorscope_probe_read_user_max_256(void *dst, __u64 size, const void *unsafe_ptr) {
    if (size >= 256) {
        return bpf_probe_read_user(dst, 256, unsafe_ptr);
    }
    return bpf_probe_read_user(dst, (__u32)size & 255,
                               unsafe_ptr);
}

static __always_inline long
censorscope_probe_read_user_max_4096(void *dst, __u64 size, const void *unsafe_ptr) {
    if (size >= 4096) {
        return bpf_probe_read_user(dst, 4096, unsafe_ptr);
    }
    return bpf_probe_read_user(dst, (__u32)size & 4095,
                               unsafe_ptr);
}

static __always_inline long
censorscope_probe_read_user_max_128(void *dst, __u64 size, const void *unsafe_ptr) {
    if (size >= 128) {
        return bpf_probe_read_user(dst, 128, unsafe_ptr);
    }
    return bpf_probe_read_user(dst, (__u32)size & 127,
                               unsafe_ptr);
}
#define CORE_READ(dst, source, field)                                    \
    bpf_probe_read_kernel((dst),                                               \
                          sizeof(*(dst)),                                      \
                          __builtin_preserve_access_index(&(source)->field))

#endif
