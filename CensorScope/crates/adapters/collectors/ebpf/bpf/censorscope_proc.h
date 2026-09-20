#ifndef CENSORSCOPE_PROC_H
#define CENSORSCOPE_PROC_H

#include "censorscope_runtime.h"

#if defined(__TARGET_ARCH_x86)
struct pt_regs {
    __u64 r15;
    __u64 r14;
    __u64 r13;
    __u64 r12;
    __u64 bp;
    __u64 bx;
    __u64 r11;
    __u64 r10;
    __u64 r9;
    __u64 r8;
    __u64 ax;
    __u64 cx;
    __u64 dx;
    __u64 si;
    __u64 di;
    __u64 orig_ax;
    __u64 ip;
    __u64 cs;
    __u64 flags;
    __u64 sp;
    __u64 ss;
};
#define UPROBE_ARG1(ctx) ((ctx)->di)
#define UPROBE_ARG2(ctx) ((ctx)->si)
#define UPROBE_ARG3(ctx) ((ctx)->dx)
#define UPROBE_ARG4(ctx) ((ctx)->cx)
#define UPROBE_RC(ctx) ((ctx)->ax)
#elif defined(__TARGET_ARCH_arm64)
struct pt_regs {
    __u64 regs[31];
    __u64 sp;
    __u64 pc;
    __u64 pstate;
};
#define UPROBE_ARG1(ctx) ((ctx)->regs[0])
#define UPROBE_ARG2(ctx) ((ctx)->regs[1])
#define UPROBE_ARG3(ctx) ((ctx)->regs[2])
#define UPROBE_ARG4(ctx) ((ctx)->regs[3])
#define UPROBE_RC(ctx) ((ctx)->regs[0])
#endif

struct censorscope_tls_capture_args {
    __u64 trace_id;
    __u64 buffer_ptr;
    __u64 requested_size;
    __u64 captured_size;
    __u64 connection_ptr;
    __u64 call_id;
    __u32 direction;
    __u32 symbol;
    __u32 extra_flags;
};

static __always_inline int emit_tls_chunk(const struct censorscope_tls_capture_args *args,
                                          __u64 chunk_offset,
                                          __u32 chunk_index,
                                          __u32 extra_flags) {
    struct censorscope_tls_event *event;
    __u64 capture_limit = args->requested_size < args->captured_size
                              ? args->requested_size
                              : args->captured_size;
    __u64 remaining = capture_limit > chunk_offset
                          ? capture_limit - chunk_offset
                          : 0;
    __u32 size = remaining > TLS_PAYLOAD_ABI_MAX_BYTES
                     ? TLS_PAYLOAD_ABI_MAX_BYTES
                     : (__u32)remaining;
    __u64 available = args->captured_size > chunk_offset
                          ? args->captured_size - chunk_offset
                          : 0;
    if (available < size) {
        size = (__u32)available;
    }
    if (size == 0 && args->extra_flags == 0) {
        return BPF_STATUS_OK;
    }
    event = censorscope_event_reserve(sizeof(*event));
    if (!event) {
        record_loss(LOSS_COUNTER_TLS);
        return BPF_STATUS_ERROR;
    }
    init_tls_event(&event->event, current_trace_tgid(args->trace_id),
                   args->trace_id);
    event->payload_size = size;
    event->event.requested_size = args->requested_size;
    event->payload_flags = args->extra_flags | extra_flags;
    event->direction = args->direction;
    event->symbol = args->symbol;
    event->connection_ptr = args->connection_ptr;
    event->call_id = args->call_id;
    event->chunk_offset = chunk_offset;
    event->chunk_index = chunk_index;
    event->chunk_flags = (chunk_offset == 0 ? TLS_CHUNK_FLAG_START : 0) |
                         (chunk_offset + size >= capture_limit
                              ? TLS_CHUNK_FLAG_END
                              : 0);
    if (size > 0 &&
        (!args->buffer_ptr ||
         censorscope_probe_read_user_max_4096(
             event->payload, size,
             (const void *)(args->buffer_ptr + chunk_offset)) != 0)) {
        event->payload_size = 0;
        event->payload_flags |= TLS_FLAG_READ_FAILURE;
    }
    censorscope_event_submit(0, event);
    return BPF_STATUS_OK;
}

/* State shared between the caller and the bpf_loop() callback.  The callback
 * receives a pointer to this struct as its second argument.  Running the loop
 * counter inside the kernel keeps the body out of the caller's control flow
 * graph, so the verifier walks it once instead of re-deriving every per-chunk
 * state for all TLS_MAX_CHUNKS iterations. */
struct censorscope_tls_loop_ctx {
    struct censorscope_tls_capture_args args;
    __u64 capture_limit;
    __u32 status;
    __u32 reserved;
};

/* One chunk per iteration.  Returns 1 to stop early: chunk offsets grow
 * monotonically, so once the offset leaves the capture window every remaining
 * iteration would be a no-op. */
static __noinline long emit_tls_chunk_iteration(__u32 index, void *loop_ctx) {
    struct censorscope_tls_loop_ctx *loop = loop_ctx;
    struct censorscope_tls_capture_args args;
    __u32 flags = 0;
    __u64 offset;

    if (index >= TLS_MAX_CHUNKS) {
        return 1;
    }
    offset = (__u64)index * TLS_PAYLOAD_ABI_MAX_BYTES;
    if (offset >= loop->capture_limit) {
        return 1;
    }
    /* Copy into this frame so everything below works on callback-local stack
     * rather than on a pointer back into the caller's frame. */
    args = loop->args;
    if (index == TLS_MAX_CHUNKS - 1 &&
        args.requested_size > offset + TLS_PAYLOAD_ABI_MAX_BYTES) {
        flags |= TLS_FLAG_TRUNCATED;
    }
    loop->status |= emit_tls_chunk(&args, offset, index, flags);
    return 0;
}

static __always_inline int
emit_tls_bytes(const struct censorscope_tls_capture_args *args) {
    struct censorscope_tls_loop_ctx loop = {};
    __u64 capture_limit = args->requested_size < args->captured_size
                              ? args->requested_size
                              : args->captured_size;

    loop.args = *args;
    loop.capture_limit = capture_limit;
    loop.status = BPF_STATUS_OK;

    if (capture_limit > 0) {
        long iterations =
            bpf_loop(TLS_MAX_CHUNKS, emit_tls_chunk_iteration, &loop, 0);
        if (iterations < 0) {
            loop.status |= BPF_STATUS_ERROR;
        }
    } else if (args->extra_flags != 0) {
        /* Nothing is in range, but the outcome (a read failure, say) still
         * has to reach the reader as one flag-only chunk. */
        loop.status |= emit_tls_chunk(args, 0, 0, 0);
    }
    return (int)loop.status;
}

static __always_inline int tls_write_enter(struct pt_regs *ctx, __u32 symbol) {
    __u64 pid_tgid = current_pid_tgid();
    __u32 pid = pid_tgid >> 32;
    __u64 *trace_id = lookup_trace_for_pid(pid);
    if (!trace_id) {
        return BPF_STATUS_OK;
    }
    struct censorscope_tls_capture_args args = {
        .trace_id = *trace_id,
        .buffer_ptr = UPROBE_ARG2(ctx),
        .requested_size = UPROBE_ARG3(ctx),
        .captured_size = UPROBE_ARG3(ctx),
        .connection_ptr = UPROBE_ARG1(ctx),
        .call_id = bpf_ktime_get_ns(),
        .direction = TLS_DIRECTION_OUTBOUND,
        .symbol = symbol,
        .extra_flags = 0,
    };
    return emit_tls_bytes(&args);
}

static __always_inline int tls_write_ex_enter(struct pt_regs *ctx, __u32 symbol) {
    __u64 pid_tgid = current_pid_tgid();
    __u32 pid = pid_tgid >> 32;
    __u64 *trace_id = lookup_trace_for_pid(pid);
    struct censorscope_pending_tls_op op = {};
    if (!trace_id) {
        return BPF_STATUS_OK;
    }
    if (bpf_map_lookup_elem(&pending_tls_ops, &pid_tgid)) {
        record_loss(LOSS_COUNTER_TLS);
        return BPF_STATUS_ERROR;
    }
    op.trace_id = *trace_id;
    op.buffer_ptr = UPROBE_ARG2(ctx);
    op.requested_size = UPROBE_ARG3(ctx);
    op.connection_ptr = UPROBE_ARG1(ctx);
    op.call_id = bpf_ktime_get_ns();
    op.result_ptr = UPROBE_ARG4(ctx);
    op.direction = TLS_DIRECTION_OUTBOUND;
    op.symbol = symbol;
    if (bpf_map_update_elem(&pending_tls_ops, &pid_tgid, &op, BPF_ANY) != 0) {
        record_loss(LOSS_COUNTER_TLS);
    }
    return BPF_STATUS_OK;
}

static __always_inline int tls_read_enter(struct pt_regs *ctx, __u32 symbol) {
    __u64 pid_tgid = current_pid_tgid();
    __u32 pid = pid_tgid >> 32;
    __u64 *trace_id = lookup_trace_for_pid(pid);
    struct censorscope_pending_tls_op op = {};
    if (!trace_id) {
        return BPF_STATUS_OK;
    }
    if (bpf_map_lookup_elem(&pending_tls_ops, &pid_tgid)) {
        /* The map is keyed by thread. A nested TLS call cannot be represented
         * safely without overwriting the outer call, so account for the loss
         * and leave the original pending operation intact. */
        record_loss(LOSS_COUNTER_TLS);
        return BPF_STATUS_ERROR;
    }
    op.trace_id = *trace_id;
    op.buffer_ptr = UPROBE_ARG2(ctx);
    op.requested_size = UPROBE_ARG3(ctx);
    op.connection_ptr = UPROBE_ARG1(ctx);
    op.call_id = bpf_ktime_get_ns();
    op.result_ptr = (symbol == TLS_SYMBOL_SSL_READ_EX) ? UPROBE_ARG4(ctx) : 0;
    op.direction = TLS_DIRECTION_INBOUND;
    op.symbol = symbol;
    if (bpf_map_update_elem(&pending_tls_ops, &pid_tgid, &op, BPF_ANY) != 0) {
        record_loss(LOSS_COUNTER_TLS);
    }
    return BPF_STATUS_OK;
}

static __always_inline int tls_read_exit(struct pt_regs *ctx) {
    __u64 pid_tgid = current_pid_tgid();
    struct censorscope_pending_tls_op *op =
        bpf_map_lookup_elem(&pending_tls_ops, &pid_tgid);
    __u64 result;
    __s64 signed_result;
    int status;
    if (!op) {
        return BPF_STATUS_OK;
    }
    result = UPROBE_RC(ctx);
    signed_result = (__s64)result;
    if (op->result_ptr && signed_result >= 0) {
        __u64 actual = 0;
        if (bpf_probe_read_user(&actual, sizeof(actual),
                                (const void *)op->result_ptr) == 0) {
            result = actual;
        } else {
            result = 0;
            signed_result = -1;
        }
    }
    if (signed_result < 0) {
        result = 0;
    }
    struct censorscope_tls_capture_args args = {
        .trace_id = op->trace_id,
        .buffer_ptr = op->buffer_ptr,
        .requested_size = op->requested_size,
        .captured_size = result,
        .connection_ptr = op->connection_ptr,
        .call_id = op->call_id,
        .direction = op->direction,
        .symbol = op->symbol,
        .extra_flags = signed_result < 0
                           ? TLS_FLAG_READ_FAILURE
                           : ((op->direction == TLS_DIRECTION_OUTBOUND &&
                               result < op->requested_size)
                                  ? TLS_FLAG_TRUNCATED
                                  : 0),
    };
    status = emit_tls_bytes(&args);
    bpf_map_delete_elem(&pending_tls_ops, &pid_tgid);
    return status;
}

static __always_inline int finalize_fork_trace_binding(__u32 child_kernel_pid) {
    struct censorscope_trace_binding *binding = lookup_binding(child_kernel_pid);

    if (!binding || binding->parent_pid == 0) {
        /* Nothing pending, or already promoted (exec'd or userspace-seeded):
         * leave untouched. */
        return BPF_STATUS_OK;
    }
    /* Promote the fork-only binding in place: clearing parent_pid makes every
     * later single-map lookup treat the pid as fully tracked.  The write is
     * unconditional, so no multi-map rollback is required. */
    binding->parent_pid = 0;
    return BPF_STATUS_OK;
}

/* Self-noise filter: return 1 when the exec filename's basename is one of the
 * daemon control tools spawned under a trace root (censorscopectl,
 * censorscoped).  The filename is stored at a +14 byte offset inside the
 * scratch buffer.  The offset is at least the longest matched name (14), so
 * the tail-window start (= offset + len - name_len) is never negative for any
 * length the clamp allows: every index stays within the 160-byte value
 * without relying on branch refinement in code generation. */
#define EXEC_NAME_TAIL_OFFSET 14
#define EXEC_NAME_STR_BYTES (EXEC_NAME_SCRATCH_BYTES - EXEC_NAME_TAIL_OFFSET)
static __always_inline int exec_basename_is_excluded(const void *filename) {
    __u32 zero = 0;
    struct censorscope_exec_name_scratch *scr =
        bpf_map_lookup_elem(&exec_name_scratch, &zero);
    long n;
    long len;

    if (!scr) {
        return 0;
    }
    n = bpf_probe_read_kernel_str(
        scr->name + EXEC_NAME_TAIL_OFFSET, EXEC_NAME_STR_BYTES, filename);
    if (n <= 1) {
        return 0;
    }
    len = n - 1;
    if (len > EXEC_NAME_STR_BYTES) {
        len = EXEC_NAME_STR_BYTES;
    }
    /* censorscopectl (14): start = TAIL_OFFSET + len - 14 = len, so the
     * window occupies [len, len + 13] inside [14, 159] for any clamped len. */
    if (len >= 14) {
        long s = EXEC_NAME_TAIL_OFFSET + len - 14;
        if (s < EXEC_NAME_TAIL_OFFSET || s + 14 > EXEC_NAME_SCRATCH_BYTES) {
            return 0;
        }
        if (scr->name[s + 0] == 'c' && scr->name[s + 1] == 'e' &&
            scr->name[s + 2] == 'n' && scr->name[s + 3] == 's' &&
            scr->name[s + 4] == 'o' && scr->name[s + 5] == 'r' &&
            scr->name[s + 6] == 's' && scr->name[s + 7] == 'c' &&
            scr->name[s + 8] == 'o' && scr->name[s + 9] == 'p' &&
            scr->name[s + 10] == 'e' && scr->name[s + 11] == 'c' &&
            scr->name[s + 12] == 't' && scr->name[s + 13] == 'l') {
            if (len == 14) {
                return 1;
            }
            if (scr->name[s - 1] == '/') {
                return 1;
            }
        }
    }
    /* censorscoped (12): start = TAIL_OFFSET + len - 12 = len + 2, so the
     * window occupies [len + 2, len + 13] inside [14, 159] for any clamped
     * len. */
    if (len >= 12) {
        long s = EXEC_NAME_TAIL_OFFSET + len - 12;
        if (s < EXEC_NAME_TAIL_OFFSET || s + 12 > EXEC_NAME_SCRATCH_BYTES) {
            return 0;
        }
        if (scr->name[s + 0] == 'c' && scr->name[s + 1] == 'e' &&
            scr->name[s + 2] == 'n' && scr->name[s + 3] == 's' &&
            scr->name[s + 4] == 'o' && scr->name[s + 5] == 'r' &&
            scr->name[s + 6] == 's' && scr->name[s + 7] == 'c' &&
            scr->name[s + 8] == 'o' && scr->name[s + 9] == 'p' &&
            scr->name[s + 10] == 'e' && scr->name[s + 11] == 'd') {
            if (len == 12) {
                return 1;
            }
            if (scr->name[s - 1] == '/') {
                return 1;
            }
        }
    }
    return 0;
}

static __always_inline int emit_exec_proc_event(
    struct sched_process_exec_ctx *ctx, __u32 pid, __u64 trace_id) {
    struct censorscope_exec_event *event;
    __u32 filename_offset;
    __u32 filename_data_size;
    long filename_size;

    event = censorscope_event_reserve(sizeof(*event));
    if (!event) {
        return BPF_STATUS_ERROR;
    }

    init_event(&event->event, PROC_EXEC, pid, trace_id);
    event->event.aux = (__u32)ctx->old_pid;
    event->filename_size = 0;
    event->filename_flags = 0;
    event->filename[0] = 0;
    event->argv_count = 0;
    event->argv_flags =
        EXEC_ARG_FLAG_PARTIAL | EXEC_ARG_FLAG_READ_FAILURE;
    event->argv_bytes_size = 0;
    event->argv_reserved = 0;

    filename_offset = ctx->filename_loc & 0xffff;
    filename_data_size = ctx->filename_loc >> 16;
    if (filename_offset) {
        const void *filename = (const void *)ctx + filename_offset;

        filename_size = bpf_probe_read_kernel_str(
            event->filename, sizeof(event->filename), filename);
        if (filename_size > 0) {
            event->filename_size = (__u32)(filename_size - 1);
            if (filename_size == sizeof(event->filename) ||
                filename_data_size > sizeof(event->filename)) {
                event->filename_flags |= EXEC_FILENAME_FLAG_TRUNCATED;
            }
        }
    }

    {
        __u64 key = current_pid_tgid();
        struct censorscope_pending_argv *argv =
            bpf_map_lookup_elem(&pending_argv, &key);
        if (argv) {
            event->argv_count = argv->argv_count;
            event->argv_flags = argv->argv_flags;
            event->argv_bytes_size = argv->argv_bytes_size;
            __builtin_memcpy(event->argv_entries,
                             argv->argv_entries,
                             sizeof(event->argv_entries));
            __builtin_memcpy(
                event->argv_bytes, argv->argv_bytes, sizeof(event->argv_bytes));
            bpf_map_delete_elem(&pending_argv, &key);
        }
    }

    censorscope_event_submit(ctx, event);
    return BPF_STATUS_OK;
}

static __always_inline int capture_argv(__u64 argv_ptr) {
    __u64 pid_tgid = current_pid_tgid();
    __u32 pid = pid_tgid >> 32;
    __u64 *trace_id = lookup_trace_for_pid(pid);
    __u32 zero = 0;
    struct censorscope_pending_argv *capture;
    int i;

    if (!trace_id) {
        return BPF_STATUS_OK;
    }
    capture = bpf_map_lookup_elem(&argv_scratch, &zero);
    if (!capture) {
        return BPF_STATUS_OK;
    }
    capture->argv_count = 0;
    capture->argv_flags = 0;
    capture->argv_bytes_size = 0;
    capture->reserved = 0;
    capture->trace_id = *trace_id;
    if (!argv_ptr) {
        capture->argv_flags =
            EXEC_ARG_FLAG_PARTIAL | EXEC_ARG_FLAG_READ_FAILURE;
    } else {
#pragma unroll
        for (i = 0; i < EXEC_ARG_MAX; i++) {
            __u64 arg_ptr = 0;
            __u32 slot_offset = i * EXEC_ARG_SLOT_ABI_MAX_BYTES;
            __u32 copied;
            long result;
            if (bpf_probe_read_user(
                    &arg_ptr,
                    sizeof(arg_ptr),
                    (const void *)(argv_ptr + ((__u64)i * sizeof(arg_ptr)))) !=
                0) {
                capture->argv_flags |= EXEC_ARG_FLAG_PARTIAL |
                                       EXEC_ARG_FLAG_READ_FAILURE;
                break;
            }
            if (!arg_ptr) {
                break;
            }
            /* A map-value destination with both a variable offset and a
             * variable helper size is rejected by older verifiers even when
             * C control flow proves offset + size is in bounds. The loop is
             * fully unrolled, so fixed per-argument slots give every helper
             * call a constant destination range and size. Entries still
             * preserve argument boundaries and the original captured bytes. */
            result = bpf_probe_read_user_str(capture->argv_bytes + slot_offset,
                                             EXEC_ARG_SLOT_ABI_MAX_BYTES,
                                             (const void *)arg_ptr);
            if (result <= 0 || result > EXEC_ARG_SLOT_ABI_MAX_BYTES) {
                capture->argv_flags |= EXEC_ARG_FLAG_PARTIAL |
                                       EXEC_ARG_FLAG_READ_FAILURE;
                break;
            }
            copied = (__u32)result - 1;
            capture->argv_entries[i].offset = slot_offset;
            capture->argv_entries[i].length = copied;
            capture->argv_bytes_size = slot_offset + copied;
            capture->argv_count = i + 1;
            if (result == EXEC_ARG_SLOT_ABI_MAX_BYTES) {
                capture->argv_flags |=
                    EXEC_ARG_FLAG_PARTIAL | EXEC_ARG_FLAG_TRUNCATED;
            }
        }
        /* Probe the next pointer so a longer argv is not silently reported
         * as complete; the capture is explicitly marked partial/truncated. */
        if (capture->argv_count == EXEC_ARG_MAX &&
            capture->argv_flags == 0) {
            __u64 next_arg_ptr = 0;
            if (bpf_probe_read_user(
                    &next_arg_ptr,
                    sizeof(next_arg_ptr),
                    (const void *)(argv_ptr + ((__u64)EXEC_ARG_MAX *
                                               sizeof(next_arg_ptr)))) != 0) {
                capture->argv_flags |= EXEC_ARG_FLAG_PARTIAL |
                                       EXEC_ARG_FLAG_READ_FAILURE;
            } else if (next_arg_ptr) {
                capture->argv_flags |=
                    EXEC_ARG_FLAG_PARTIAL | EXEC_ARG_FLAG_TRUNCATED;
            }
        }
    }
    if (bpf_map_update_elem(&pending_argv, &pid_tgid, capture, BPF_ANY) != 0) {
        /* key 1 is the collector-wide counter projected as a trace-scoped
         * loss event by the daemon; argv map loss must not be silent. */
        record_loss(LOSS_COUNTER_EXEC_CONTEXT);
    }
    return BPF_STATUS_OK;
}

static __always_inline int
clear_failed_argv(struct trace_event_raw_sys_exit *ctx) {
    if (ctx->ret < 0) {
        __u64 key = current_pid_tgid();
        bpf_map_delete_elem(&pending_argv, &key);
    }
    return BPF_STATUS_OK;
}

static __noinline int
store_pending_exit_op(struct trace_event_raw_sys_enter *ctx) {
    __u64 pid_tgid = current_pid_tgid();
    __u32 pid = pid_tgid >> 32;
    __u64 *trace_id = 0;
    struct censorscope_pending_exit_op op = {};

    if (pid) {
        trace_id = lookup_trace_for_pid(pid);
    }
    if (!trace_id) {
        __u64 kernel_pid_tgid = current_kernel_pid_tgid();
        __u32 kernel_pid = kernel_pid_tgid >> 32;

        if (kernel_pid_tgid && kernel_pid_tgid != pid_tgid) {
            trace_id = lookup_trace_for_pid(kernel_pid);
            if (trace_id) {
                pid_tgid = kernel_pid_tgid;
                pid = kernel_pid;
            }
        }
    }
    if (!pid || !trace_id) {
        return BPF_STATUS_OK;
    }

    op.code = (__s32)ctx->args[0];
    bpf_map_update_elem(&pending_exit_ops, &pid_tgid, &op, BPF_ANY);
    return BPF_STATUS_OK;
}

static __always_inline void attach_exit_code(struct censorscope_event *event,
                                             __u64 pid_tgid) {
    struct censorscope_pending_exit_op *op =
        bpf_map_lookup_elem(&pending_exit_ops, &pid_tgid);

    if (!op) {
        return;
    }
    event->aux = (__u32)op->code;
    event->result = 1;
    bpf_map_delete_elem(&pending_exit_ops, &pid_tgid);
}

#endif
