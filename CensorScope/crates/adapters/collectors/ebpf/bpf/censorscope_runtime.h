#ifndef CENSORSCOPE_RUNTIME_H
#define CENSORSCOPE_RUNTIME_H

#include "include/censorscope_const.h"
#include "censorscope_helpers.h"

#ifndef BPF_ANY
#define BPF_ANY 0
#endif

enum censorscope_proc_event_kind {
    PROC_FORK = 1,
    PROC_EXEC = 2,
    PROC_EXIT = 3,
};

enum censorscope_net_event_kind {
    NET_CONNECT = 1,
    NET_ACCEPT = 2,
    NET_SEND = 3,
    NET_RECV = 4,
    NET_BIND = 5,
    NET_LISTEN = 6,
    NET_SENDMSG = 7,
    NET_FD_WRITE = 8,
    NET_FD_READ = 9,
    NET_FD_WRITEV = 10,
    NET_FD_READV = 11,
};

enum censorscope_file_event_kind {
    FILE_OPEN = 1,
    FILE_OPENAT = 2,
    FILE_CREAT = 3,
    FILE_UNLINKAT = 4,
    FILE_RENAMEAT = 5,
    FILE_MKDIRAT = 6,
    FILE_MMAP = 7,
    FILE_CLOSE = 8,
    FILE_CLOSE_RANGE = 9,
    FILE_DUP = 10,
    FILE_DUP2 = 11,
    FILE_DUP3 = 12,
    FILE_FCNTL = 13,
    FILE_CHDIR = 14,
    FILE_FCHDIR = 15,
    FILE_READ = 16,
    FILE_WRITE = 17,
    FILE_READV = 18,
    FILE_WRITEV = 19,
    FILE_RMDIR = 20,
    FILE_OPENAT2 = 21,
    FILE_TRUNCATE = 22,
    FILE_FTRUNCATE = 23,
    /* Path-change family on the dirfd-less legacy syscalls and the modern
     * renameat2. */
    FILE_RENAMEAT2 = 24,
    FILE_RENAME = 25,
    FILE_UNLINK = 26,
    FILE_MKDIR = 27,
};

enum censorscope_ipc_event_kind {
    IPC_PIPE = 1,
    IPC_SOCKETPAIR = 2,
};

enum censorscope_stdio_direction {
    STDIO_INBOUND = 1,
    STDIO_OUTBOUND = 2,
};

enum censorscope_proc_event_flag {
    PROC_FORK_CHILD_HOST_ONLY = 1,
    PROC_FORK_PARENT_HOST_ONLY = 2,
};

enum censorscope_trace_lookup_flag {
    TRACE_LOOKUP_FLAG_HOST_FALLBACK = 1,
};

enum censorscope_event_kind {
    EVENT_NET = 4,
    EVENT_FILE = 5,
    EVENT_IPC = 6,
    EVENT_SIGNAL = 8,
    EVENT_TLS = 9,
    /* Merged fd-io event for read/write/readv/writev on any fd; the
     * userspace decoder fans one record out to file, socket, or stdio
     * observations from the fd state it maintains. */
    EVENT_FD_IO = 10,
};

/* Operations carried by the merged fd-io event (event.aux, offset 8). */
enum censorscope_fd_io_operation {
    FD_IO_READ = 1,
    FD_IO_WRITE = 2,
    FD_IO_READV = 3,
    FD_IO_WRITEV = 4,
};

enum censorscope_bpf_status {
    BPF_STATUS_OK = 0,
    BPF_STATUS_ERROR = -1,
};

enum censorscope_bpf_flag {
    BPF_FLAG_NONE = 0,
};

enum censorscope_loss_counter_key {
    LOSS_COUNTER_DEFAULT = 1,
};

enum censorscope_capture_flag {
    CAPTURE_FLAG_READ_FAILURE = 1,
};

enum censorscope_tls_symbol {
    TLS_SYMBOL_SSL_WRITE = 1,
    TLS_SYMBOL_SSL_READ = 2,
    TLS_SYMBOL_GNUTLS_SEND = 3,
    TLS_SYMBOL_GNUTLS_RECV = 4,
    TLS_SYMBOL_NSS_WRITE = 5,
    TLS_SYMBOL_NSS_READ = 6,
    TLS_SYMBOL_GO_WRITE = 7,
    TLS_SYMBOL_GO_READ = 8,
    TLS_SYMBOL_RUSTLS_WRITE = 9,
    TLS_SYMBOL_RUSTLS_READ = 10,
    TLS_SYMBOL_SSL_WRITE_EX = 11,
    TLS_SYMBOL_SSL_READ_EX = 12,
};

struct censorscope_event {
    __u32 kind;
    __u32 pid;
    __u32 aux;
    __u32 host_pid;
    __u32 aux_host_pid;
    __s32 result;
    __u64 trace_id;
    __u64 observed_ktime_ns;
    __u32 fd;
    __u32 reserved;
    __u64 requested_size;
    __u64 pid_generation;
    __u64 aux_generation;
    /* Captured at event time into fixed-size ABI fields so short-lived
     * children remain attributable after their /proc entry disappears. */
    char session[SESSION_LEN];
    char call_id[CALL_ID_LEN];
} __attribute__((packed));

struct censorscope_exec_event {
    struct censorscope_event event;
    __u32 filename_size;
    __u32 filename_flags;
    char filename[EXEC_FILENAME_ABI_MAX_BYTES];
    __u32 argv_count;
    __u32 argv_flags;
    __u32 argv_bytes_size;
    __u32 argv_reserved;
    struct {
        __u32 offset;
        __u32 length;
    } argv_entries[EXEC_ARG_MAX];
    __u8 argv_bytes[EXEC_ARG_BYTES_ABI_MAX];
} __attribute__((packed));

struct censorscope_pending_argv {
    __u64 trace_id;
    __u32 argv_count;
    __u32 argv_flags;
    __u32 argv_bytes_size;
    __u32 reserved;
    struct {
        __u32 offset;
        __u32 length;
    } argv_entries[EXEC_ARG_MAX];
    __u8 argv_bytes[EXEC_ARG_BYTES_ABI_MAX];
};

struct censorscope_pending_tls_op {
    __u64 trace_id;
    __u64 buffer_ptr;
    __u64 requested_size;
    __u64 connection_ptr;
    __u64 call_id;
    __u64 result_ptr;
    __u32 direction;
    __u32 symbol;
};

struct censorscope_tls_event {
    struct censorscope_event event;
    __u32 payload_size;
    __u32 payload_flags;
    __u32 direction;
    __u32 symbol;
    __u64 connection_ptr;
    __u64 call_id;
    __u64 chunk_offset;
    __u32 chunk_index;
    __u32 chunk_flags;
    __u8 payload[TLS_PAYLOAD_ABI_MAX_BYTES];
} __attribute__((packed));

/* One entry per tracked host PID.  A fork-created binding keeps the parent
 * host pid in `parent_pid` until exec promotes the child in place (cleared);
 * userspace-seeded bindings are inserted promoted.  `child_generation` is
 * the process start_boottime, disambiguating PID reuse.  Field order keeps
 * trace_id 8-byte aligned for map-value pointer reads. */
struct censorscope_trace_binding {
    __u64 trace_id;
    __u64 child_generation;
    __u32 parent_pid;
};

struct censorscope_pending_exit_op {
    __s32 code;
};

struct censorscope_pending_net_op {
    __u64 trace_id;
    __u32 operation;
    __u32 fd;
    __u64 requested_size;
    __u64 sockaddr_ptr;
    __u32 sockaddr_len;
    __u64 sockaddr_len_ptr;
};

/* Leading fields of the 64-bit Linux user_msghdr ABI (same on x86_64 and
 * aarch64); only msg_name/msg_namelen are needed for endpoint observation. */
struct censorscope_user_msghdr_prefix {
    __u64 msg_name;
    __u32 msg_namelen;
    __u32 padding;
};

struct censorscope_net_event {
    struct censorscope_event event;
    __u32 sockaddr_size;
    __u32 sockaddr_flags;
    __u8 sockaddr[128];
} __attribute__((packed));

struct censorscope_pending_file_op {
    __u64 trace_id;
    __u32 operation;
    __u32 fd;
    __u64 requested_size;
    __u64 path_ptr;
    __u64 path2_ptr;
};

struct censorscope_file_event {
    struct censorscope_event event;
    __u32 path_size;
    __u32 path_flags;
    __u32 path2_size;
    __u32 path2_flags;
    char path[FILE_PATH_ABI_MAX_BYTES];
    char path2[FILE_PATH_ABI_MAX_BYTES];
} __attribute__((packed));

struct censorscope_pending_ipc_op {
    __u64 trace_id;
    __u64 pair_ptr;
    __u32 operation;
    __u32 domain;
};

/* Pending state for the merged fd-io probes.  Payload is captured only for
 * stdio fds (0..2): write data is staged at syscall entry so the buffer
 * stays valid; read data is fetched from the user buffer at exit. */
struct censorscope_fd_io_pending_op {
    __u64 trace_id;
    __u64 buffer_ptr;
    __u64 requested_size;
    __u32 fd;
    __u32 operation;
    __u32 staged_size;
    __u32 staged_flags;
    __u8 staged_payload[256];
};

struct censorscope_fd_io_event {
    struct censorscope_event event;
    __u32 payload_size;
    __u32 payload_flags;
    __u8 payload[256];
} __attribute__((packed));

struct task_struct {
    int pid;
    int tgid;
    __u64 start_boottime;
    struct mm_struct *mm;
} __attribute__((preserve_access_index));

struct mm_struct {
    unsigned long env_start;
    unsigned long env_end;
} __attribute__((preserve_access_index));

struct tracepoint_common {
    __u16 common_type;
    __u8 common_flags;
    __u8 common_preempt_count;
    __s32 common_pid;
};

struct sched_process_exec_ctx {
    struct tracepoint_common common;
    __u32 filename_loc;
    __s32 pid;
    __s32 old_pid;
};

struct sched_process_exit_ctx {
    struct tracepoint_common common;
    char comm[16];
    __s32 pid;
    __s32 prio;
};

struct signal_generate_ctx {
    struct tracepoint_common common;
    __s32 sig;
    __s32 error;
    __s32 code;
    char comm[16];
    __s32 pid;
    __s32 group;
    __s32 signal_result;
};

struct trace_event_raw_sys_enter {
    struct tracepoint_common common;
    long id;
    unsigned long args[6];
};

struct trace_event_raw_sys_exit {
    struct tracepoint_common common;
    long id;
    long ret;
};

/* One map covers promoted processes (parent_pid == 0) and fork-pending
 * children of a tracked parent (parent_pid != 0); userspace seeds it with
 * `tracked_process_max_entries` entries. */
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct censorscope_trace_binding);
} trace_bindings SEC(".maps");

/* Session identity cached per process lifetime: TLS uprobe programs (whose
 * payload loop is already verifier-heavy) skip the bounded userspace env
 * scan, while forked descendants inherit the identity before their first
 * syscall. */
struct censorscope_session_value {
    char value[SESSION_LEN];
};

struct censorscope_env_scratch {
    char value[ENV_BUF_LEN];
};

/* Per-CPU exec-name scratch for the self-noise filter: the exec filename
 * basename is compared against the excluded tools (censorscopectl/
 * censorscoped); kept off the BPF stack. */
#define EXEC_NAME_SCRATCH_BYTES 160
struct censorscope_exec_name_scratch {
    char name[EXEC_NAME_SCRATCH_BYTES];
};

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct censorscope_exec_name_scratch);
} exec_name_scratch SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct censorscope_session_value);
} session_ids SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct censorscope_session_value);
} call_ids SEC(".maps");

/* Env discovery runs from several nested programs; keep the 256-byte read
 * buffer off the BPF stack.  Programs cannot migrate CPUs while executing,
 * so one per-CPU scratch value is sufficient. */
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct censorscope_env_scratch);
} env_scratch SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1);
    __type(key, __u64);
    __type(value, struct censorscope_pending_exit_op);
} pending_exit_ops SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1);
    __type(key, __u64);
    __type(value, struct censorscope_pending_argv);
} pending_argv SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct censorscope_pending_argv);
} argv_scratch SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1);
    __type(key, __u64);
    __type(value, struct censorscope_pending_tls_op);
} pending_tls_ops SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1);
    __type(key, __u64);
    __type(value, struct censorscope_pending_net_op);
} pending_net_ops SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1);
    __type(key, __u64);
    __type(value, struct censorscope_pending_file_op);
} pending_file_ops SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1);
    __type(key, __u64);
    __type(value, struct censorscope_pending_ipc_op);
} pending_ipc_ops SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1);
    __type(key, __u64);
    __type(value, struct censorscope_fd_io_pending_op);
} pending_fd_io_ops SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 1);
} events SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 8);
    __type(key, __u32);
    __type(value, __u64);
} loss_counters SEC(".maps");

static __always_inline __u64 current_kernel_pid_tgid(void) {
    return bpf_get_current_pid_tgid();
}

static __always_inline __u32 current_kernel_tgid(void) {
    return current_kernel_pid_tgid() >> 32;
}

static __always_inline __u64 current_pid_tgid(void) {
    return current_kernel_pid_tgid();
}

static __always_inline __u32 current_tgid(void) {
    return current_kernel_tgid();
}

static __always_inline __u32 current_trace_tgid(__u64 trace_id) {
    return current_kernel_tgid();
}

static __always_inline struct censorscope_trace_binding *lookup_binding(__u32 pid) {
    return pid ? bpf_map_lookup_elem(&trace_bindings, &pid) : (void *)0;
}

/* Single-pid trace id with no fork-pending fallback: pid already has an
 * image (TLS uprobes) or the caller does its own host-pid fallback. */
static __always_inline __u64 *lookup_trace_for_pid(__u32 pid) {
    struct censorscope_trace_binding *binding = lookup_binding(pid);
    return binding ? &binding->trace_id : (__u64 *)0;
}

static __always_inline __u64 *
lookup_current_trace(__u32 *tgid, __u32 *tid, __u32 *flags) {
    __u64 pid_tgid = current_kernel_pid_tgid();
    __u32 pid = pid_tgid >> 32;
    struct censorscope_trace_binding *binding = lookup_binding(pid);
    *tgid = pid;
    *tid = (__u32)pid_tgid;
    *flags = 0;
    if (binding) {
        /* Fork-pending child (parent_pid != 0): resolve via the same entry,
         * flagged for host-fallback attribution. */
        if (binding->parent_pid != 0) {
            *flags = TRACE_LOOKUP_FLAG_HOST_FALLBACK;
        }
        return &binding->trace_id;
    }
    return (__u64 *)0;
}

static __always_inline __u64 *lookup_trace_for_context_pid(__u32 context_pid,
                                                           __u32 *tgid,
                                                           __u32 *tid,
                                                           __u32 *flags) {
    __u64 *trace_id = lookup_current_trace(tgid, tid, flags);
    if (trace_id || !context_pid || context_pid == *tgid) {
        return trace_id;
    }
    struct censorscope_trace_binding *binding = lookup_binding(context_pid);
    if (binding) {
        *tgid = context_pid;
        *tid = context_pid;
        *flags = TRACE_LOOKUP_FLAG_HOST_FALLBACK;
        return &binding->trace_id;
    }
    return (__u64 *)0;
}

static __always_inline __u64 current_process_start_time(__u32 pid) {
    struct censorscope_trace_binding *binding = lookup_binding(pid);
    return binding ? binding->child_generation : 0;
}

static __always_inline void delete_trace_binding(__u32 pid) {
    if (pid) {
        bpf_map_delete_elem(&trace_bindings, &pid);
    }
}

static __always_inline void cache_session(__u32 pid,
                                          const char *session) {
    if (!pid || !session || !session[0])
        return;
    /* The map value is just the fixed-size array, so event->session is
     * ABI-compatible with no extra stack temporary. */
    bpf_map_update_elem(&session_ids, &pid, session, BPF_ANY);
}

static __noinline void copy_cached_session(struct censorscope_event *event,
                                           __u32 pid) {
    struct censorscope_session_value *value;
    if (!pid)
        return;
    value = bpf_map_lookup_elem(&session_ids, &pid);
    if (value)
        __builtin_memcpy(event->session, value->value,
                         sizeof(event->session));
}

static __always_inline void cache_call_id(__u32 pid, const char *call_id) {
    if (!pid || !call_id || !call_id[0])
        return;
    bpf_map_update_elem(&call_ids, &pid, call_id, BPF_ANY);
}

static __noinline void copy_cached_call_id(struct censorscope_event *event,
                                           __u32 pid) {
    struct censorscope_session_value *value;
    if (!pid)
        return;
    value = bpf_map_lookup_elem(&call_ids, &pid);
    if (value)
        __builtin_memcpy(event->call_id, value->value,
                         sizeof(event->call_id));
}

/* Read the agent-provided SessionId from the current task's env at
 * observation time (no later /proc read).  Kept a separate BPF subprogram:
 * inlining this scan into TLS payload probes would multiply verifier state
 * with the payload chunk loop. */
static __noinline void find_session(struct censorscope_event *event,
                                    struct task_struct *task) {
    struct mm_struct *mm = 0;
    struct censorscope_env_scratch *scratch;
    unsigned long cursor;
    unsigned long end;
    __u32 zero = 0;
    if (!task || CORE_READ(&mm, task, mm) != 0 || !mm)
        return;
    if (CORE_READ(&cursor, mm, env_start) != 0 ||
        CORE_READ(&end, mm, env_end) != 0 || !cursor || cursor >= end)
        return;
    scratch = bpf_map_lookup_elem(&env_scratch, &zero);
    if (!scratch)
        return;
#pragma clang loop unroll(disable)
    for (int i = 0; i < MAX_ENV_VARS; i++) {
        long n;
        if (cursor >= end)
            break;
        n = bpf_probe_read_user_str(scratch->value, sizeof(scratch->value),
                                    (const void *)cursor);
        if (n <= 1)
            break;
        __u32 value_offset = 0;
        if (scratch->value[0] == 'C' && scratch->value[1] == 'E' &&
            scratch->value[2] == 'N' && scratch->value[3] == 'S' &&
            scratch->value[4] == 'O' && scratch->value[5] == 'R' &&
            scratch->value[6] == 'S' && scratch->value[7] == 'C' &&
            scratch->value[8] == 'O' && scratch->value[9] == 'P' &&
            scratch->value[10] == 'E' && scratch->value[11] == '_' &&
            scratch->value[12] == 'S' && scratch->value[13] == 'E' &&
            scratch->value[14] == 'S' && scratch->value[15] == 'S' &&
            scratch->value[16] == 'I' && scratch->value[17] == 'O' &&
            scratch->value[18] == 'N' && scratch->value[19] == '_' &&
            scratch->value[20] == 'I' && scratch->value[21] == 'D' &&
            scratch->value[22] == '=' && scratch->value[23] != '\0') {
            value_offset = 23;
        } else if (scratch->value[0] == 'D' && scratch->value[1] == 'S' &&
                   scratch->value[2] == 'H' && scratch->value[3] == '_' &&
                   scratch->value[4] == 'S' && scratch->value[5] == 'E' &&
                   scratch->value[6] == 'S' && scratch->value[7] == 'S' &&
                   scratch->value[8] == 'I' && scratch->value[9] == 'O' &&
                   scratch->value[10] == 'N' && scratch->value[11] == '_' &&
                   scratch->value[12] == 'I' && scratch->value[13] == 'D' &&
                   scratch->value[14] == '=' && scratch->value[15] != '\0') {
            value_offset = 15;
        }
        if (value_offset != 0) {
#pragma clang loop unroll(full)
            for (int j = 0; j < SESSION_LEN - 1; j++) {
                char c = scratch->value[j + value_offset];
                event->session[j] = c;
                if (!c)
                    break;
            }
            break;
        }
        cursor += n;
    }
}

/* Per-tool Harness CallId, read while current (mirrors find_session) and
 * cached per process for later events, including TLS uprobes. */
static __noinline void find_call_id(struct censorscope_event *event,
                                    struct task_struct *task) {
    struct mm_struct *mm = 0;
    struct censorscope_env_scratch *scratch;
    unsigned long cursor;
    unsigned long end;
    __u32 zero = 0;
    if (!task || CORE_READ(&mm, task, mm) != 0 || !mm)
        return;
    if (CORE_READ(&cursor, mm, env_start) != 0 ||
        CORE_READ(&end, mm, env_end) != 0 || !cursor || cursor >= end)
        return;
    scratch = bpf_map_lookup_elem(&env_scratch, &zero);
    if (!scratch)
        return;
#pragma clang loop unroll(disable)
    for (int i = 0; i < MAX_ENV_VARS; i++) {
        long n;
        if (cursor >= end)
            break;
        n = bpf_probe_read_user_str(scratch->value, sizeof(scratch->value),
                                    (const void *)cursor);
        if (n <= 1)
            break;
        int value_offset = 0;
        if (scratch->value[0] == 'D' && scratch->value[1] == 'S' &&
            scratch->value[2] == 'H' && scratch->value[3] == '_' &&
            scratch->value[4] == 'C' && scratch->value[5] == 'E' &&
            scratch->value[6] == 'N' && scratch->value[7] == 'S' &&
            scratch->value[8] == 'O' && scratch->value[9] == 'R' &&
            scratch->value[10] == 'S' && scratch->value[11] == 'C' &&
            scratch->value[12] == 'O' && scratch->value[13] == 'P' &&
            scratch->value[14] == 'E' && scratch->value[15] == '_' &&
            scratch->value[16] == 'C' && scratch->value[17] == 'A' &&
            scratch->value[18] == 'L' && scratch->value[19] == 'L' &&
            scratch->value[20] == '_' && scratch->value[21] == 'I' &&
            scratch->value[22] == 'D' && scratch->value[23] == '=') {
            value_offset = 24;
        }
        if (value_offset != 0) {
#pragma clang loop unroll(full)
            for (int j = 0; j < CALL_ID_LEN - 1; j++) {
                char c = scratch->value[j + value_offset];
                event->call_id[j] = c;
                if (!c)
                    break;
            }
            break;
        }
        cursor += n;
    }
}

static __always_inline void init_event_from_cache(struct censorscope_event *event,
                                                  __u32 kind,
                                                  __u32 pid,
                                                  __u64 trace_id) {
    __builtin_memset(event, 0, sizeof(*event));
    event->kind = kind;
    event->pid = current_trace_tgid(trace_id);
    event->host_pid = pid;
    event->trace_id = trace_id;
    event->observed_ktime_ns = bpf_ktime_get_ns();
    event->pid_generation = current_process_start_time(pid);
    copy_cached_session(event, current_kernel_tgid());
    copy_cached_call_id(event, current_kernel_tgid());
}

static __always_inline void
init_event(struct censorscope_event *event, __u32 kind, __u32 pid, __u64 trace_id) {
    init_event_from_cache(event, kind, pid, trace_id);
    if (!event->session[0]) {
        find_session(event, bpf_get_current_task());
        cache_session(current_kernel_tgid(), event->session);
    }
    if (!event->call_id[0]) {
        find_call_id(event, bpf_get_current_task());
        cache_call_id(current_kernel_tgid(), event->call_id);
    }
}

/* TLS payload probes never scan task memory for identities; earlier
 * process/syscall events populate these caches. */
static __always_inline void init_tls_event(struct censorscope_event *event,
                                           __u32 pid,
                                           __u64 trace_id) {
    init_event_from_cache(event, EVENT_TLS, pid, trace_id);
}

static __always_inline void *censorscope_event_reserve(__u64 size) {
    return bpf_ringbuf_reserve(&events, size, 0);
}

static __always_inline void censorscope_event_submit(void *ctx, void *event) {
    bpf_ringbuf_submit(event, 0);
}

static __always_inline void record_loss(__u32 key) {
    __u64 *count = bpf_map_lookup_elem(&loss_counters, &key);
    __u64 next = count ? *count + 1 : LOSS_COUNTER_DEFAULT;
    bpf_map_update_elem(&loss_counters, &key, &next, BPF_ANY);
}

static __always_inline int emit_event(void *ctx, struct censorscope_event *event) {
    long result = bpf_ringbuf_output(&events, event, sizeof(*event),
                                     BPF_FLAG_NONE);
    if (result != 0) {
        __u32 key = LOSS_COUNTER_DEFAULT;
        __u64 *count = bpf_map_lookup_elem(&loss_counters, &key);
        __u64 next = count ? *count + 1 : LOSS_COUNTER_DEFAULT;
        bpf_map_update_elem(&loss_counters, &key, &next, BPF_ANY);
    }
    return result;
}

static __always_inline int
store_pending_net_op(struct trace_event_raw_sys_enter *ctx,
                     __u32 operation,
                     __u32 fd,
                     __u64 requested_size) {
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_pending_net_op op = {};
    if (!trace_id || !tgid) {
        return BPF_STATUS_OK;
    }
    op.trace_id = *trace_id;
    op.operation = operation;
    op.fd = fd;
    op.requested_size = requested_size;
    op.sockaddr_ptr = 0;
    op.sockaddr_len = 0;
    op.sockaddr_len_ptr = 0;
    return bpf_map_update_elem(&pending_net_ops, &pid_tgid, &op, BPF_ANY);
}

static __always_inline int
store_pending_net_sockaddr_op(struct trace_event_raw_sys_enter *ctx,
                              __u32 operation,
                              __u32 fd,
                              __u64 requested_size,
                              __u64 sockaddr_ptr,
                              __u32 sockaddr_len,
                              __u64 sockaddr_len_ptr) {
    int result = store_pending_net_op(ctx, operation, fd, requested_size);
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_pending_net_op *op =
        bpf_map_lookup_elem(&pending_net_ops, &pid_tgid);
    if (result == 0 && trace_id && op) {
        op->sockaddr_ptr = sockaddr_ptr;
        op->sockaddr_len = sockaddr_len;
        op->sockaddr_len_ptr = sockaddr_len_ptr;
    }
    return result;
}

static __always_inline int
store_pending_net_sendmsg_op(struct trace_event_raw_sys_enter *ctx) {
    __u64 message_ptr = (__u64)ctx->args[1];
    struct censorscope_user_msghdr_prefix message = {};

    if (!message_ptr) {
        return store_pending_net_op(
            ctx, NET_SENDMSG, (__u32)ctx->args[0], 0);
    }
    if (bpf_probe_read_user(
            &message, sizeof(message), (const void *)message_ptr) != 0) {
        /* Non-null pointer with zero length: an explicit endpoint capture
         * gap is emitted by emit_pending_net_op. */
        return store_pending_net_sockaddr_op(
            ctx, NET_SENDMSG, (__u32)ctx->args[0], 0, message_ptr, 0, 0);
    }
    return store_pending_net_sockaddr_op(ctx,
                                         NET_SENDMSG,
                                         (__u32)ctx->args[0],
                                         0,
                                         message.msg_name,
                                         message.msg_namelen,
                                         0);
}

static __always_inline int
emit_pending_net_op(struct trace_event_raw_sys_exit *ctx) {
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_pending_net_op *op =
        bpf_map_lookup_elem(&pending_net_ops, &pid_tgid);
    struct censorscope_net_event *event;
    if (!trace_id || !op) {
        return BPF_STATUS_OK;
    }
    event = censorscope_event_reserve(sizeof(*event));
    if (!event) {
        record_loss(LOSS_COUNTER_DEFAULT);
        bpf_map_delete_elem(&pending_net_ops, &pid_tgid);
        return BPF_STATUS_ERROR;
    }
    init_event(&event->event, EVENT_NET, tgid, op->trace_id);
    event->event.aux = op->operation;
    event->event.fd = op->fd;
    event->event.requested_size = op->requested_size;
    event->event.result = (__s32)ctx->ret;
    if (op->operation == NET_ACCEPT && ctx->ret >= 0) {
        event->event.fd = (__u32)ctx->ret;
    }
    event->sockaddr_size = 0;
    event->sockaddr_flags = 0;
    __u32 sockaddr_len = op->sockaddr_len;
    if (sockaddr_len == 0 && op->sockaddr_len_ptr) {
        bpf_probe_read_user(&sockaddr_len,
                            sizeof(sockaddr_len),
                            (const void *)op->sockaddr_len_ptr);
    }
    if (sockaddr_len > sizeof(event->sockaddr)) {
        sockaddr_len = sizeof(event->sockaddr);
    }
    if (op->sockaddr_ptr && sockaddr_len > 0 &&
        censorscope_probe_read_user_max_128(event->sockaddr,
                                      sockaddr_len,
                                      (const void *)op->sockaddr_ptr) == 0) {
        event->sockaddr_size = sockaddr_len;
    } else if (op->sockaddr_ptr) {
        event->sockaddr_flags = CAPTURE_FLAG_READ_FAILURE;
    }
    censorscope_event_submit(ctx, event);
    bpf_map_delete_elem(&pending_net_ops, &pid_tgid);
    return BPF_STATUS_OK;
}

static __always_inline int
store_pending_file_op(struct trace_event_raw_sys_enter *ctx,
                      __u32 operation,
                      __u32 fd,
                      __u64 requested_size) {
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_pending_file_op op = {};
    if (!trace_id || !tgid) {
        return BPF_STATUS_OK;
    }
    op.trace_id = *trace_id;
    op.operation = operation;
    op.fd = fd;
    op.requested_size = requested_size;
    op.path_ptr = 0;
    op.path2_ptr = 0;
    return bpf_map_update_elem(&pending_file_ops, &pid_tgid, &op, BPF_ANY);
}

static __always_inline int
store_pending_file_path_op(struct trace_event_raw_sys_enter *ctx,
                           __u32 operation,
                           __u32 fd,
                           __u64 path_ptr,
                           __u64 path2_ptr) {
    int result = store_pending_file_op(ctx, operation, fd, 0);
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_pending_file_op *op =
        bpf_map_lookup_elem(&pending_file_ops, &pid_tgid);
    if (result == 0 && trace_id && op) {
        op->path_ptr = path_ptr;
        op->path2_ptr = path2_ptr;
    }
    return result;
}

static __always_inline int
store_pending_file_two_path_op(struct trace_event_raw_sys_enter *ctx,
                               __u32 operation,
                               __u32 dirfd,
                               __u64 path_ptr,
                               __u32 target_dirfd,
                               __u64 path2_ptr) {
    int result =
        store_pending_file_path_op(ctx, operation, dirfd, path_ptr, path2_ptr);
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_pending_file_op *op =
        bpf_map_lookup_elem(&pending_file_ops, &pid_tgid);
    if (result == 0 && trace_id && op) {
        op->requested_size = target_dirfd;
    }
    return result;
}

static __always_inline int
emit_pending_file_op(struct trace_event_raw_sys_exit *ctx) {
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_pending_file_op *op =
        bpf_map_lookup_elem(&pending_file_ops, &pid_tgid);
    struct censorscope_file_event *event;
    if (!trace_id || !op) {
        return BPF_STATUS_OK;
    }
    event = censorscope_event_reserve(sizeof(*event));
    if (!event) {
        record_loss(LOSS_COUNTER_DEFAULT);
        bpf_map_delete_elem(&pending_file_ops, &pid_tgid);
        return BPF_STATUS_ERROR;
    }
    init_event(&event->event, EVENT_FILE, tgid, op->trace_id);
    event->event.aux = op->operation;
    event->event.fd = op->fd;
    event->event.reserved = op->fd;
    event->event.requested_size = op->requested_size;
    event->event.result = (__s32)ctx->ret;
    if ((op->operation == FILE_OPEN ||
         op->operation == FILE_OPENAT ||
         op->operation == FILE_OPENAT2 ||
         op->operation == FILE_CREAT) &&
        ctx->ret >= 0) {
        event->event.fd = (__u32)ctx->ret;
    }
    if (op->operation == FILE_DUP && ctx->ret >= 0) {
        event->event.requested_size = (__u64)ctx->ret;
    }
    event->path_size = 0;
    event->path_flags = 0;
    event->path2_size = 0;
    event->path2_flags = 0;
    if (op->path_ptr) {
        long size = bpf_probe_read_user_str(
            event->path, sizeof(event->path), (const void *)op->path_ptr);
        if (size > 0) {
            event->path_size = (__u32)(size - 1);
            if (size == sizeof(event->path)) {
                event->path_flags = FILE_PATH_FLAG_TRUNCATED;
            }
        } else {
            event->path_flags = FILE_PATH_FLAG_CAPTURE_GAP;
        }
    }
    if (op->path2_ptr) {
        long size2 = bpf_probe_read_user_str(
            event->path2, sizeof(event->path2), (const void *)op->path2_ptr);
        if (size2 > 0) {
            event->path2_size = (__u32)(size2 - 1);
            if (size2 == sizeof(event->path2)) {
                event->path2_flags = FILE_PATH_FLAG_TRUNCATED;
            }
        } else {
            event->path2_flags = FILE_PATH_FLAG_CAPTURE_GAP;
        }
    }
    censorscope_event_submit(ctx, event);
    bpf_map_delete_elem(&pending_file_ops, &pid_tgid);
    return BPF_STATUS_OK;
}

static __always_inline int
store_pending_ipc_op(struct trace_event_raw_sys_enter *ctx,
                     __u32 operation,
                     __u64 pair_ptr,
                     __u32 domain) {
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_pending_ipc_op op = {};
    if (!trace_id || !tgid) {
        return BPF_STATUS_OK;
    }
    op.trace_id = *trace_id;
    op.operation = operation;
    op.pair_ptr = pair_ptr;
    op.domain = domain;
    return bpf_map_update_elem(&pending_ipc_ops, &pid_tgid, &op, BPF_ANY);
}

static __always_inline int
emit_pending_ipc_op(struct trace_event_raw_sys_exit *ctx) {
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_pending_ipc_op *op =
        bpf_map_lookup_elem(&pending_ipc_ops, &pid_tgid);
    struct censorscope_event event = {};
    __s32 pair[2] = {};
    if (!trace_id || !op) {
        return BPF_STATUS_OK;
    }
    if (ctx->ret != 0 ||
        bpf_probe_read_user(pair, sizeof(pair), (const void *)op->pair_ptr) !=
            0) {
        bpf_map_delete_elem(&pending_ipc_ops, &pid_tgid);
        return BPF_STATUS_OK;
    }
    init_event(&event, EVENT_IPC, tgid, op->trace_id);
    event.aux = op->operation;
    event.fd = (__u32)pair[0];
    event.reserved = (__u32)pair[1];
    event.requested_size = op->domain;
    event.result = (__s32)ctx->ret;
    emit_event(ctx, &event);
    bpf_map_delete_elem(&pending_ipc_ops, &pid_tgid);
    return BPF_STATUS_OK;
}

static __always_inline int
store_pending_fd_io_op(struct trace_event_raw_sys_enter *ctx,
                       __u32 operation,
                       __u32 fd,
                       __u64 buffer_ptr,
                       __u64 requested_size) {
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_fd_io_pending_op op = {};
    if (!trace_id || !tgid) {
        return BPF_STATUS_OK;
    }
    op.trace_id = *trace_id;
    op.fd = fd;
    op.operation = operation;
    op.buffer_ptr = buffer_ptr;
    op.requested_size = requested_size;
    /* Stdio payload staging for write() on fds 0..2 only; read() payload is
     * captured at exit from the user buffer, readv/writev carry none. */
    if (fd <= 2 && operation == FD_IO_WRITE && buffer_ptr && requested_size) {
        __u32 staged_size;
        if (requested_size >= sizeof(op.staged_payload)) {
            staged_size = sizeof(op.staged_payload);
        } else {
            staged_size = (__u32)requested_size;
        }
        if (censorscope_probe_read_user_max_256(op.staged_payload,
                                          staged_size,
                                          (const void *)buffer_ptr) == 0) {
            op.staged_size = staged_size;
        } else {
            op.staged_flags = CAPTURE_FLAG_READ_FAILURE;
        }
    }
    return bpf_map_update_elem(&pending_fd_io_ops, &pid_tgid, &op, BPF_ANY);
}

static __always_inline int
emit_pending_fd_io_op(struct trace_event_raw_sys_exit *ctx) {
    __u32 tgid = 0;
    __u32 tid = 0;
    __u32 flags = 0;
    __u64 *trace_id = lookup_current_trace(&tgid, &tid, &flags);
    __u64 pid_tgid = ((__u64)tgid << 32) | tid;
    struct censorscope_fd_io_pending_op *op =
        bpf_map_lookup_elem(&pending_fd_io_ops, &pid_tgid);
    struct censorscope_fd_io_event *event;
    if (!trace_id || !op) {
        return BPF_STATUS_OK;
    }
    event = censorscope_event_reserve(sizeof(*event));
    if (!event) {
        record_loss(LOSS_COUNTER_DEFAULT);
        bpf_map_delete_elem(&pending_fd_io_ops, &pid_tgid);
        return BPF_STATUS_ERROR;
    }
    init_event(&event->event, EVENT_FD_IO, tgid, op->trace_id);
    event->event.aux = op->operation;
    event->event.fd = op->fd;
    event->event.requested_size = op->requested_size;
    event->event.result = (__s32)ctx->ret;
    event->payload_size = 0;
    event->payload_flags = 0;
    if (ctx->ret > 0 && op->fd <= 2) {
        __u32 capture_size;
        if ((__u64)ctx->ret >= sizeof(event->payload)) {
            capture_size = sizeof(event->payload);
        } else {
            capture_size = (__u32)ctx->ret;
        }
        if (op->operation == FD_IO_WRITE) {
            if (capture_size > op->staged_size) {
                capture_size = op->staged_size;
            }
            if (op->staged_flags || !capture_size) {
                event->payload_flags = CAPTURE_FLAG_READ_FAILURE;
            } else {
                __builtin_memcpy(
                    event->payload, op->staged_payload, sizeof(event->payload));
                event->payload_size = capture_size;
            }
        } else if (op->operation == FD_IO_READ && op->buffer_ptr) {
            if (censorscope_probe_read_user_max_256(event->payload,
                                              capture_size,
                                              (const void *)op->buffer_ptr) !=
                0) {
                event->payload_flags = CAPTURE_FLAG_READ_FAILURE;
            } else {
                event->payload_size = capture_size;
            }
        }
    }
    censorscope_event_submit(ctx, event);
    bpf_map_delete_elem(&pending_fd_io_ops, &pid_tgid);
    return BPF_STATUS_OK;
}

#endif
