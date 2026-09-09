#include "censorscope_proc.h"

SEC("uprobe")
int handle_tls_ssl_write_enter(struct pt_regs *ctx) {
    return tls_write_enter(ctx, TLS_SYMBOL_SSL_WRITE);
}

SEC("uprobe")
int handle_tls_ssl_write_ex_enter(struct pt_regs *ctx) {
    return tls_write_ex_enter(ctx, TLS_SYMBOL_SSL_WRITE_EX);
}

SEC("uretprobe")
int handle_tls_ssl_write_ex_exit(struct pt_regs *ctx) {
    return tls_read_exit(ctx);
}

SEC("uprobe")
int handle_tls_ssl_read_enter(struct pt_regs *ctx) {
    return tls_read_enter(ctx, TLS_SYMBOL_SSL_READ);
}

SEC("uprobe")
int handle_tls_ssl_read_exit(struct pt_regs *ctx) {
    return tls_read_exit(ctx);
}

SEC("uprobe")
int handle_tls_ssl_read_ex_enter(struct pt_regs *ctx) {
    return tls_read_enter(ctx, TLS_SYMBOL_SSL_READ_EX);
}

SEC("uretprobe")
int handle_tls_ssl_read_ex_exit(struct pt_regs *ctx) {
    return tls_read_exit(ctx);
}

/* GnuTLS and NSS pass the plaintext buffer in the same first three argument
 * positions; probes stay separate so symbol/library coverage stays explicit
 * in diagnostics and payload metadata. */
SEC("uprobe")
int handle_tls_gnutls_send_enter(struct pt_regs *ctx) {
    return tls_write_enter(ctx, TLS_SYMBOL_GNUTLS_SEND);
}

SEC("uprobe")
int handle_tls_gnutls_recv_enter(struct pt_regs *ctx) {
    return tls_read_enter(ctx, TLS_SYMBOL_GNUTLS_RECV);
}

SEC("uprobe")
int handle_tls_gnutls_recv_exit(struct pt_regs *ctx) {
    return tls_read_exit(ctx);
}

SEC("uprobe")
int handle_tls_nss_write_enter(struct pt_regs *ctx) {
    return tls_write_enter(ctx, TLS_SYMBOL_NSS_WRITE);
}

SEC("uprobe")
int handle_tls_nss_read_enter(struct pt_regs *ctx) {
    return tls_read_enter(ctx, TLS_SYMBOL_NSS_READ);
}

SEC("uprobe")
int handle_tls_nss_read_exit(struct pt_regs *ctx) {
    return tls_read_exit(ctx);
}

/* Go crypto/tls probes use the register ABI layout exposed by the supported
 * cgo/ABI0 entry points; unsupported ABI variants stay unresolved and cannot
 * claim coverage. */
SEC("uprobe")
int handle_tls_go_write_enter(struct pt_regs *ctx) {
    return tls_write_enter(ctx, TLS_SYMBOL_GO_WRITE);
}

SEC("uprobe")
int handle_tls_go_read_enter(struct pt_regs *ctx) {
    return tls_read_enter(ctx, TLS_SYMBOL_GO_READ);
}

SEC("uretprobe")
int handle_tls_go_read_exit(struct pt_regs *ctx) {
    return tls_read_exit(ctx);
}

/* Rustls read_tls/write_tls wrappers are handled through the same passive
 * ABI; mangled Rust symbols are resolved by the userspace symbol matcher. */
SEC("uprobe")
int handle_tls_rustls_write_enter(struct pt_regs *ctx) {
    return tls_write_enter(ctx, TLS_SYMBOL_RUSTLS_WRITE);
}

SEC("uprobe")
int handle_tls_rustls_read_enter(struct pt_regs *ctx) {
    return tls_read_enter(ctx, TLS_SYMBOL_RUSTLS_READ);
}

SEC("uretprobe")
int handle_tls_rustls_read_exit(struct pt_regs *ctx) {
    return tls_read_exit(ctx);
}

SEC("raw_tracepoint/sched_process_fork")
int handle_sched_process_fork(struct bpf_raw_tracepoint_args *ctx) {
    struct task_struct *parent_task = (struct task_struct *)ctx->args[0];
    struct task_struct *child_task = (struct task_struct *)ctx->args[1];
    __u32 parent_pid = 0;
    __u32 parent_tid = 0;
    __u32 lookup_flags = 0;
    __u32 context_parent_pid = 0;
    __u32 parent_host_pid = 0;
    __u32 child_host_pid = 0;
    __u64 child_start_boottime_ns = 0;
    __u64 inherited_trace_id = 0;
    __u64 parent_start_ns = 0;
    if (!parent_task || !child_task) {
        return BPF_STATUS_OK;
    }
    if (CORE_READ(&context_parent_pid, parent_task, pid) != 0 ||
        CORE_READ(&parent_host_pid, parent_task, tgid) != 0 ||
        CORE_READ(&child_host_pid, child_task, tgid) != 0 ||
        CORE_READ(&child_start_boottime_ns, child_task, start_boottime) !=
            0) {
        return BPF_STATUS_OK;
    }
    __u64 *trace_id = lookup_trace_for_context_pid(
        context_parent_pid, &parent_pid, &parent_tid, &lookup_flags);
    struct censorscope_trace_binding child_binding = {};
    struct censorscope_event event = {};
    __u32 child_kernel_pid = child_host_pid;

    if (trace_id) {
        inherited_trace_id = *trace_id;
    } else {
        struct censorscope_trace_binding *parent_binding = 0;

        /* Host-tgid fallback for tracepoints whose context pid is a
         * namespace-local value.  Skip it when it would only repeat the
         * lookup that just missed. */
        if (parent_host_pid != context_parent_pid &&
            parent_host_pid != current_kernel_tgid()) {
            parent_binding = lookup_binding(parent_host_pid);
        }
        if (parent_binding) {
            inherited_trace_id = parent_binding->trace_id;
            parent_pid = parent_host_pid;
            lookup_flags = TRACE_LOOKUP_FLAG_HOST_FALLBACK;
        }
    }

    if (!parent_pid || !inherited_trace_id) {
        return BPF_STATUS_OK;
    }
    if (!child_kernel_pid || !child_start_boottime_ns) {
        return BPF_STATUS_OK;
    }
    if (child_host_pid == parent_host_pid) {
        return BPF_STATUS_OK;
    }

    parent_start_ns = current_process_start_time(parent_pid);
    child_binding.trace_id = inherited_trace_id;
    child_binding.child_generation = child_start_boottime_ns;
    child_binding.parent_pid = parent_pid;

    /* sched_process_fork runs before wake_up_new_task().  Publish the child
     * binding here so its first post-fork syscall is already controlled. */
    if (bpf_map_update_elem(
            &trace_bindings, &child_kernel_pid, &child_binding, BPF_ANY) != 0) {
        return BPF_STATUS_OK;
    }
    init_event(&event, PROC_FORK, parent_pid, inherited_trace_id);
    /* The child inherits the harness environment at fork.  Copy the
     * parent's captured identity immediately so even a first-event TLS
     * uprobe is attributable before sched_process_exec promotes the binding. */
    cache_session(child_kernel_pid, event.session);
    cache_call_id(child_kernel_pid, event.call_id);
    event.aux = 0;
    event.reserved = PROC_FORK_CHILD_HOST_ONLY;
    if (lookup_flags & TRACE_LOOKUP_FLAG_HOST_FALLBACK) {
        event.reserved |= PROC_FORK_PARENT_HOST_ONLY;
    }
    event.host_pid = parent_host_pid;
    event.aux_host_pid = child_host_pid;
    event.pid_generation = parent_start_ns;
    event.aux_generation = child_start_boottime_ns;
    return emit_event(ctx, &event);
}

SEC("tracepoint/sched/sched_process_exec")
int handle_sched_process_exec(struct sched_process_exec_ctx *ctx) {
    __u32 pid = 0;
    __u32 tid = 0;
    __u32 lookup_flags = 0;
    __u32 context_pid = (__u32)ctx->old_pid;
    __u64 *trace_id =
        lookup_trace_for_context_pid(context_pid, &pid, &tid, &lookup_flags);

    if (!pid) {
        return BPF_STATUS_OK;
    }
    finalize_fork_trace_binding(current_kernel_tgid());
    trace_id =
        lookup_trace_for_context_pid(context_pid, &pid, &tid, &lookup_flags);
    if (!trace_id) {
        return BPF_STATUS_OK;
    }

    /* Self-noise filter: exec into a daemon control tool (censorscopectl /
     * censorscoped) removes the trace binding so this process and its syscalls
     * (daemon-socket IPC, DB reads, cache writes) are never captured.  The
     * fork event emitted before this exec stays; no exec/exit events follow. */
    if (ctx->filename_loc & 0xffff) {
        const void *filename =
            (const void *)ctx + (ctx->filename_loc & 0xffff);
        if (exec_basename_is_excluded(filename)) {
            bpf_map_delete_elem(&trace_bindings, &pid);
            return BPF_STATUS_OK;
        }
    }

    return emit_exec_proc_event(ctx, pid, *trace_id);
}

SEC("tracepoint/syscalls/sys_enter_execve")
int handle_sys_enter_execve(struct trace_event_raw_sys_enter *ctx) {
    /* Keep the tracepoint context access at a verifier-known constant offset:
     * a variable ctx->args[index] dereference is rejected as a modified
     * tracepoint context pointer. */
    return capture_argv((__u64)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_enter_execveat")
int handle_sys_enter_execveat(struct trace_event_raw_sys_enter *ctx) {
    return capture_argv((__u64)ctx->args[3]);
}

SEC("tracepoint/syscalls/sys_exit_execve")
int handle_sys_exit_execve(struct trace_event_raw_sys_exit *ctx) {
    return clear_failed_argv(ctx);
}

SEC("tracepoint/syscalls/sys_exit_execveat")
int handle_sys_exit_execveat(struct trace_event_raw_sys_exit *ctx) {
    return clear_failed_argv(ctx);
}

SEC("tracepoint/sched/sched_process_exit")
int handle_sched_process_exit(struct sched_process_exit_ctx *ctx) {
    __u32 pid = 0;
    __u32 tid = 0;
    __u32 lookup_flags = 0;
    __u64 pid_tgid;
    __u64 kernel_pid_tgid = current_kernel_pid_tgid();
    __u64 *trace_id;
    __u32 context_pid = (__u32)ctx->pid;
    __u32 host_pid = kernel_pid_tgid >> 32;
    __u32 host_tid = (__u32)kernel_pid_tgid;
    struct censorscope_event event;
    struct censorscope_trace_binding *binding;

    trace_id =
        lookup_trace_for_context_pid(context_pid, &pid, &tid, &lookup_flags);
    pid_tgid = ((__u64)pid << 32) | tid;
    if (!pid) {
        /* Reap a fork-pending binding that never resolved through an event.
         * Promoted bindings are left alone: their exit normally resolves. */
        if (host_pid && host_pid == host_tid) {
            binding = lookup_binding(host_pid);
            if (binding && binding->parent_pid != 0) {
                bpf_map_delete_elem(&trace_bindings, &host_pid);
            }
        }
        return BPF_STATUS_OK;
    }
    if (pid != tid) {
        return BPF_STATUS_OK;
    }
    if (!trace_id) {
        finalize_fork_trace_binding(host_pid);
        trace_id = lookup_trace_for_context_pid(
            context_pid, &pid, &tid, &lookup_flags);
    }
    if (!trace_id) {
        if (host_pid) {
            binding = lookup_binding(host_pid);
            if (binding && binding->parent_pid != 0) {
                bpf_map_delete_elem(&trace_bindings, &host_pid);
            }
        }
        return BPF_STATUS_OK;
    }
    init_event(&event, PROC_EXIT, pid, *trace_id);
    attach_exit_code(&event, pid_tgid);
    emit_event(ctx, &event);
    bpf_map_delete_elem(&trace_bindings, &pid);
    if (host_pid && host_pid != pid) {
        bpf_map_delete_elem(&trace_bindings, &host_pid);
    }
    bpf_map_delete_elem(&session_ids, &host_pid);
    bpf_map_delete_elem(&call_ids, &host_pid);
    return BPF_STATUS_OK;
}

SEC("tracepoint/syscalls/sys_enter_exit")
int handle_sys_enter_exit(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_exit_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_exit_group")
int handle_sys_enter_exit_group(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_exit_op(ctx);
}

SEC("tracepoint/signal/signal_generate")
int handle_signal_generate(struct signal_generate_ctx *ctx) {
    __u32 pid = 0;
    __u32 tid = 0;
    __u32 lookup_flags = 0;
    __u64 *trace_id = lookup_current_trace(&pid, &tid, &lookup_flags);
    struct censorscope_event event = {};

    if (!pid || !trace_id) {
        return BPF_STATUS_OK;
    }
    init_event(&event, EVENT_SIGNAL, pid, *trace_id);
    event.result = ctx->signal_result;
    event.fd = (__u32)ctx->sig;
    event.reserved = (__u32)ctx->group;
    event.requested_size = (__u64)ctx->pid;
    return emit_event(ctx, &event);
}

SEC("tracepoint/syscalls/sys_enter_connect")
int handle_sys_enter_connect(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_net_sockaddr_op(ctx,
                                         NET_CONNECT,
                                         (__u32)ctx->args[0],
                                         0,
                                         (__u64)ctx->args[1],
                                         (__u32)ctx->args[2],
                                         0);
}

SEC("tracepoint/syscalls/sys_exit_connect")
int handle_sys_exit_connect(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_net_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_accept")
int handle_sys_enter_accept(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_net_sockaddr_op(ctx,
                                         NET_ACCEPT,
                                         (__u32)ctx->args[0],
                                         0,
                                         (__u64)ctx->args[1],
                                         0,
                                         (__u64)ctx->args[2]);
}

SEC("tracepoint/syscalls/sys_exit_accept")
int handle_sys_exit_accept(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_net_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_accept4")
int handle_sys_enter_accept4(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_net_sockaddr_op(ctx,
                                         NET_ACCEPT,
                                         (__u32)ctx->args[0],
                                         0,
                                         (__u64)ctx->args[1],
                                         0,
                                         (__u64)ctx->args[2]);
}

SEC("tracepoint/syscalls/sys_exit_accept4")
int handle_sys_exit_accept4(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_net_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_sendto")
int handle_sys_enter_sendto(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_net_sockaddr_op(ctx,
                                         NET_SEND,
                                         (__u32)ctx->args[0],
                                         (__u64)ctx->args[2],
                                         (__u64)ctx->args[4],
                                         (__u32)ctx->args[5],
                                         0);
}

SEC("tracepoint/syscalls/sys_exit_sendto")
int handle_sys_exit_sendto(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_net_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_sendmsg")
int handle_sys_enter_sendmsg(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_net_sendmsg_op(ctx);
}

SEC("tracepoint/syscalls/sys_exit_sendmsg")
int handle_sys_exit_sendmsg(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_net_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_recvfrom")
int handle_sys_enter_recvfrom(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_net_sockaddr_op(ctx,
                                         NET_RECV,
                                         (__u32)ctx->args[0],
                                         (__u64)ctx->args[2],
                                         (__u64)ctx->args[4],
                                         0,
                                         (__u64)ctx->args[5]);
}

SEC("tracepoint/syscalls/sys_exit_recvfrom")
int handle_sys_exit_recvfrom(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_net_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_bind")
int handle_sys_enter_bind(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_net_sockaddr_op(ctx,
                                         NET_BIND,
                                         (__u32)ctx->args[0],
                                         0,
                                         (__u64)ctx->args[1],
                                         (__u32)ctx->args[2],
                                         0);
}

SEC("tracepoint/syscalls/sys_exit_bind")
int handle_sys_exit_bind(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_net_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_listen")
int handle_sys_enter_listen(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_net_op(ctx, NET_LISTEN, (__u32)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_listen")
int handle_sys_exit_listen(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_net_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_open")
int handle_sys_enter_open(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_OPEN, (__u32)-1, (__u64)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_open")
int handle_sys_exit_open(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_openat")
int handle_sys_enter_openat(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_OPENAT, (__u32)ctx->args[0], (__u64)ctx->args[1], 0);
}

SEC("tracepoint/syscalls/sys_exit_openat")
int handle_sys_exit_openat(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_creat")
int handle_sys_enter_creat(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_CREAT, (__u32)-1, (__u64)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_creat")
int handle_sys_exit_creat(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_unlinkat")
int handle_sys_enter_unlinkat(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_UNLINKAT, (__u32)ctx->args[0], (__u64)ctx->args[1], 0);
}

SEC("tracepoint/syscalls/sys_exit_unlinkat")
int handle_sys_exit_unlinkat(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_renameat")
int handle_sys_enter_renameat(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_two_path_op(ctx,
                                          FILE_RENAMEAT,
                                          (__u32)ctx->args[0],
                                          (__u64)ctx->args[1],
                                          (__u32)ctx->args[2],
                                          (__u64)ctx->args[3]);
}

SEC("tracepoint/syscalls/sys_exit_renameat")
int handle_sys_exit_renameat(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_mkdirat")
int handle_sys_enter_mkdirat(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_MKDIRAT, (__u32)ctx->args[0], (__u64)ctx->args[1], 0);
}

SEC("tracepoint/syscalls/sys_exit_mkdirat")
int handle_sys_exit_mkdirat(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

/* renameat2 shares renameat's argument layout (olddirfd, oldpath, newdirfd,
 * newpath); the trailing flags argument is deliberately not captured. */
SEC("tracepoint/syscalls/sys_enter_renameat2")
int handle_sys_enter_renameat2(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_two_path_op(ctx,
                                          FILE_RENAMEAT2,
                                          (__u32)ctx->args[0],
                                          (__u64)ctx->args[1],
                                          (__u32)ctx->args[2],
                                          (__u64)ctx->args[3]);
}

SEC("tracepoint/syscalls/sys_exit_renameat2")
int handle_sys_exit_renameat2(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_rename")
int handle_sys_enter_rename(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_two_path_op(ctx,
                                          FILE_RENAME,
                                          (__u32)-1,
                                          (__u64)ctx->args[0],
                                          0,
                                          (__u64)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_exit_rename")
int handle_sys_exit_rename(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_unlink")
int handle_sys_enter_unlink(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_UNLINK, (__u32)-1, (__u64)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_unlink")
int handle_sys_exit_unlink(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_mkdir")
int handle_sys_enter_mkdir(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_MKDIR, (__u32)-1, (__u64)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_mkdir")
int handle_sys_exit_mkdir(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_mmap")
int handle_sys_enter_mmap(struct trace_event_raw_sys_enter *ctx) {
    /* Only executable file-backed mappings can introduce a probe target. */
    if ((__s32)ctx->args[4] < 0 || ((__u32)ctx->args[3] & 0x20) != 0 ||
        ((__u32)ctx->args[2] & 0x4) == 0) {
        return BPF_STATUS_OK;
    }
    return store_pending_file_op(
        ctx, FILE_MMAP, (__u32)ctx->args[4], (__u64)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_exit_mmap")
int handle_sys_exit_mmap(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_close")
int handle_sys_enter_close(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_op(ctx, FILE_CLOSE, (__u32)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_close")
int handle_sys_exit_close(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_close_range")
int handle_sys_enter_close_range(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_op(
        ctx, FILE_CLOSE_RANGE, (__u32)ctx->args[0], (__u64)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_exit_close_range")
int handle_sys_exit_close_range(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_dup")
int handle_sys_enter_dup(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_op(ctx, FILE_DUP, (__u32)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_dup")
int handle_sys_exit_dup(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_dup2")
int handle_sys_enter_dup2(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_op(
        ctx, FILE_DUP2, (__u32)ctx->args[0], (__u64)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_exit_dup2")
int handle_sys_exit_dup2(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_dup3")
int handle_sys_enter_dup3(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_op(
        ctx, FILE_DUP3, (__u32)ctx->args[0], (__u64)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_exit_dup3")
int handle_sys_exit_dup3(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_fcntl")
int handle_sys_enter_fcntl(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_op(
        ctx, FILE_FCNTL, (__u32)ctx->args[0], (__u64)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_exit_fcntl")
int handle_sys_exit_fcntl(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_chdir")
int handle_sys_enter_chdir(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_CHDIR, 0, (__u64)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_chdir")
int handle_sys_exit_chdir(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_fchdir")
int handle_sys_enter_fchdir(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_op(
        ctx, FILE_FCHDIR, (__u32)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_fchdir")
int handle_sys_exit_fchdir(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_pipe")
int handle_sys_enter_pipe(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_ipc_op(ctx, IPC_PIPE, (__u64)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_pipe")
int handle_sys_exit_pipe(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_ipc_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_pipe2")
int handle_sys_enter_pipe2(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_ipc_op(
        ctx, IPC_PIPE, (__u64)ctx->args[0], (__u32)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_exit_pipe2")
int handle_sys_exit_pipe2(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_ipc_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_socketpair")
int handle_sys_enter_socketpair(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_ipc_op(
        ctx, IPC_SOCKETPAIR, (__u64)ctx->args[3], (__u32)ctx->args[0]);
}

SEC("tracepoint/syscalls/sys_exit_socketpair")
int handle_sys_exit_socketpair(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_ipc_op(ctx);
}

/* One merged probe per syscall direction records a single pending op and
 * emits one EVENT_FD_IO record; userspace fans the record out to
 * file/socket/stdio observations using the fd state it already tracks. */
#define DEFINE_FD_IO_PROBES(suffix, enter_name, exit_name, operation) \
SEC("tracepoint/syscalls/" enter_name) \
int handle_sys_##suffix##_fdio(struct trace_event_raw_sys_enter *ctx) { \
    return store_pending_fd_io_op(ctx, operation, (__u32)ctx->args[0], (__u64)ctx->args[1], (__u64)ctx->args[2]); \
} \
SEC("tracepoint/syscalls/" exit_name) \
int handle_sys_##suffix##_fdio_exit(struct trace_event_raw_sys_exit *ctx) { \
    return emit_pending_fd_io_op(ctx); \
}

DEFINE_FD_IO_PROBES(read, "sys_enter_read", "sys_exit_read", FD_IO_READ)
DEFINE_FD_IO_PROBES(write, "sys_enter_write", "sys_exit_write", FD_IO_WRITE)
DEFINE_FD_IO_PROBES(readv, "sys_enter_readv", "sys_exit_readv", FD_IO_READV)
DEFINE_FD_IO_PROBES(writev, "sys_enter_writev", "sys_exit_writev", FD_IO_WRITEV)

SEC("tracepoint/syscalls/sys_enter_rmdir")
int handle_sys_enter_rmdir(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_RMDIR, (__u32)-1, (__u64)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_rmdir")
int handle_sys_exit_rmdir(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_openat2")
int handle_sys_enter_openat2(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_OPENAT2, (__u32)ctx->args[0], (__u64)ctx->args[1], 0);
}

SEC("tracepoint/syscalls/sys_exit_openat2")
int handle_sys_exit_openat2(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_truncate")
int handle_sys_enter_truncate(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_path_op(
        ctx, FILE_TRUNCATE, (__u32)-1, (__u64)ctx->args[0], 0);
}

SEC("tracepoint/syscalls/sys_exit_truncate")
int handle_sys_exit_truncate(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

SEC("tracepoint/syscalls/sys_enter_ftruncate")
int handle_sys_enter_ftruncate(struct trace_event_raw_sys_enter *ctx) {
    return store_pending_file_op(
        ctx, FILE_FTRUNCATE, (__u32)ctx->args[0], (__u64)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_exit_ftruncate")
int handle_sys_exit_ftruncate(struct trace_event_raw_sys_exit *ctx) {
    return emit_pending_file_op(ctx);
}

char LICENSE[] SEC("license") = "Dual BSD/GPL";
