// SPDX-License-Identifier: GPL-2.0
/*
 * Migration baseline: ohmyguard feat/pidtree-domain
 * commit fa1f28546abecd01fe07c0e14bad3aad1d077070.
 *
 * During parity phases this file intentionally keeps the verified hook semantics intact. Safety
 * enhancements are introduced only after the Rust/C integration matrix has a matching regression
 * test, so loader, policy compiler and kernel behavior are not changed simultaneously.
 */
/*
 * enforce.bpf.c —— 进程树作用域 (pidtree) + 多域策略 (map-in-map) eBPF 拦截
 *
 * [pidtree] P1: 作用域从 cgroup 换成自建进程树 (tracked_pids, 照搬
 * AcTrail 追踪集合维护机制), cgroup 路径已停用、代码以注释保留待回溯。
 * 设计文档: docs/design/多Agent域模型-进程树作用域设计.md
 *
 * [P2] 多域策略: 策略从全局扁平 map 换成 map-in-map 域索引
 * (docs/design/ohmyguard多域策略map-in-map改造实现方案.md):
 *   进程 → tracked_pids 拿 scope_id → scope_policies 拿 policy_slot
 *        → 7 张 outer (ARRAY_OF_MAPS) 按 slot 拿 inner 指针
 *        → inner 查规则。slot 0 固定为全局基线 (__base__), 与域 slot 并查。
 * 热更新 = 构建新 inner → 灌入 → outer 槽位原子替换 → 更新 slot_meta,
 * 无"清表重灌"中间态; 内容相同的策略组共享同一 inner (多 slot 同 fd);
 * 每条规则 value 与 slot_meta 携带策略版本, 事件上报版本供审计对账。
 *
 * 三个策略：文件 / 命令(含参数级) / 网络
 * 29 个 program: lsm.s/file_open (按 f_flags 区分读/写),
 *              lsm/file_permission (预打开 fd 实际读写时重新授权),
 *              lsm/path_truncate (写), lsm/path_unlink (删),
 *              lsm/path_rmdir (删目录), lsm/path_rename (改名/移动),
 *              等 18 个文件钩子 + lsm/bprm_check_security,
 *              tracepoint/syscalls/sys_enter_execve/execveat (取 argv 暂存),
 *              lsm/socket_connect + 守护四钩 + 进程树追踪三钩
 *
 * 注意: path_truncate/unlink/rmdir/rename 不在内核 sleepable LSM
 * 钩子名单里 (inode 锁上下文), 只能用非 sleepable lsm/ 挂载;
 * 而 bpf_d_path 在非 sleepable 程序里被禁
 * ("helper call is not allowed in probe"), 所以文件策略的判定
 * 不依赖路径字符串, 改用 (dev,ino) 身份匹配: 受害者自身 inode 查
 * file_ino 维, 祖先目录走链查 dir_ino 维。
 * file_open 是 sleepable, 仍用 bpf_d_path 拿全路径做事件展示和
 * 字符串兜底 (加载时不存在的路径只能这样拦 open)。
 *
 * 加载方式: Rust kernel crate 经 libbpf C shim 按严格 manifest 加载并 attach，
 * 任一必需 program 失败都不进入 READY。
 *
 * 循环: 全部用 #pragma unroll + 编译期常量边界, 运行时上界写成条件分支
 * (避免 break 让 clang 放弃 unroll)。
 */
#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>

char LICENSE[] SEC("license") = "GPL";

#define PATH_MAX_LEN       256
#define DIR_MAX_LEN        32
#define CMD_MAX_LEN        16
#define PATH_STR_MAX       16
#define PATH_LENS_MAX      8
#define FIXED_MAX          8
#define BUILTIN_ALLOW_N    33

/* ============ 规则 key 定义 ============
 *
 * P1 的扁平 hash map (file_blacklist / cmd_blacklist / net_rules 等)
 * 已在 P2 废弃, 改为下方 "多域策略 map-in-map" 一节的 inner/outer 结构;
 * key 结构原样保留, 作为各维度 inner map 的 key。
 */

/* 文件路径 key: 完整路径 (最长 64 字节) */
struct path_key {
    char s[64];
};

/* 命令 key: 完整命令路径 (最长 64 字节) */
struct cmd_key {
    char s[64];
};

/* 网络规则 key (LPM_TRIE, 无端口规则): prefixlen + addr 位串。
 * trie 对 key 数据按位 (MSB first) 做最长前缀匹配:
 *   prefixlen = N   -> CIDR 网段, 所有端口   ("10.0.0.0/8")
 *   prefixlen = 32  -> 单 IP, 所有端口       ("1.2.3.4")
 *   prefixlen = 0   -> 匹配一切 (白名单模式的 deny 0.0.0.0/0 兜底)
 * addr: 内存字节序 IPv4 (与 sockaddr_in.sin_addr 内存布局一致,
 *       Rust ABI 层用 LittleEndian 打包);
 * port 字段在查询 key 里参与 48 位比较, 但本表条目 prefixlen ≤ 32,
 * 端口位永远不被比较 (查 {48, addr, dport} 只为复用同一 key 结构)。 */
struct net_lpm_key {
    __u32 prefixlen;
    __u32 addr;
    __u16 port;
    __u16 pad;
};

/* 网络端口规则 key (LPM_TRIE, 带端口规则): prefixlen + {port, addr} 位串。
 *
 * 为什么需要第二张表、且 port 必须在前: "网段+端口" (CIDR:port) 规则
 * 要匹配 "addr 前 N 位 (N<32) + 端口 16 位"。若 port 放在 32 位 addr 之后
 * (net_lpm_key 布局), 这两段中间隔着 addr 的低位, trie 只支持连续前缀,
 * 表达不了 (曾按 prefixlen=N+16 实现, 实际匹配 "addr 前 N+16 位",
 * 语义完全错误)。port 在前则 "端口 16 位 + addr 前 N 位" 天然连续:
 *   prefixlen = 16+N  -> CIDR 单端口   ("10.0.0.0/8:443" -> 24)
 *   prefixlen = 16+32 -> 单 IP 单端口  ("1.2.3.4:8080"   -> 48)
 * port: 主机字节序; addr: 内存字节序 (同 net_lpm_key 约定)。 */
struct net_port_lpm_key {
    __u32 prefixlen;
    __u16 port;     /* 主机字节序 */
    __u8  addr[4];  /* 内存字节序 */
};

/* IPv6 无端口规则: prefixlen + 128 位网络序地址。 */
struct net6_lpm_key {
    __u32 prefixlen;
    __u8  addr[16];
};

/* IPv6 带端口规则: 与 IPv4 相同，端口放在地址前，prefixlen=16+CIDR。 */
struct net6_port_lpm_key {
    __u32 prefixlen;
    __u16 port;
    __u8  addr[16];
    __u16 pad;
};

/* (dev, ino) 文件/目录规则 key。
 *
 * 为什么用 (dev,ino) 不用路径字符串 (详见 docs/archive/诊断记录s.md 记录 7-10):
 * path_truncate/unlink/rmdir/rename 不在内核 sleepable LSM 名单里,
 * 只能非 sleepable 挂载; 而 bpf_d_path 在非 sleepable 程序里被禁,
 * 全路径在这些钩子里**根本拿不到**。但 dentry->d_inode->i_ino /
 * i_sb->s_dev 是普通内存读 (BPF_CORE_READ), 任何钩子都能用。
 *
 * 红利: 保护跟随 inode —— mv 改名后依然被拦; 硬链接共享 inode 天然覆盖;
 * 无路径长度/深度/挂载点限制。
 *
 * dev 编码: 内核 s_dev 是原始 dev_t ((major<<20)|minor), 用户态 stat 的
 * st_dev 是 new_encode_dev 编码, Rust 策略编译器打包 key 前做逆变换。 */
struct ino_key {
    __u64 dev;
    __u64 ino;
};

/* exec 参数 token: 最多 4 个定长槽, 每槽 32 字节 (31 字符 + NUL) */
#define ARG_TOKEN_N    4
#define ARG_TOKEN_LEN  32
#define ARG_INODE_MARKER 0xff
#define ARG_INODE_OFFSET 8

/* 参数级黑名单 key: 按 token 定长槽精确匹配, 空槽全零。
 * Rust 策略编译器把规则按空格切成 token 填槽。 */
struct arg_key {
    char tok[ARG_TOKEN_N][ARG_TOKEN_LEN];
};

/* sys_enter_execve 暂存的 argv: 4 个定长 token 槽 */
struct exec_args {
    char tok[ARG_TOKEN_N][ARG_TOKEN_LEN];
};

/* ============ 规则 value 定义 (P2: 全部携带策略版本) ============
 *
 * version 由控制面构建 inner map 时烧入, 事件上报带回用户态,
 * 支撑审计对账 ("这次判定依据的是哪一版策略") 与旧 inner 精确回收。 */

/* ino 类规则 value: 掩码 + 版本 */
struct ino_rule_val {
    __u8  mask;        /* DENY_* 掩码 */
    __u8  action;      /* 0=deny, 1=allow */
    __u8  audit;       /* 1=emit event even when allowed */
    __u8  pad;
    __u32 version;
};

/* 字符串/命令/参数类规则 value: file_str 维 mask 为 DENY_* 掩码;
 * cmd/arg 维 mask 恒 1 (存在即命中) */
struct str_rule_val {
    __u8  mask;
    __u8  action;
    __u8  audit;
    __u8  pad;
    __u32 version;
};

/* 网络规则 value */
struct net_rule_val {
    __u8  action;          /* 0 = deny, 1 = allow */
    __u8  audit;
    __u8  pad[2];
    __u32 forbid_labels;   /* IFC 预留, 当前恒 0 不参与判定 */
    __u32 version;         /* P2 新增 */
};

/* 文件策略模式 */
#define FILE_MODE_ALLOW    0  /* 白名单模式: 只允许指定路径 */
#define FILE_MODE_DENY     1  /* 黑名单模式: 只拦截指定路径 */

/* ============ 文件操作权限掩码 (file_str/file_ino/dir_ino 维规则 value 的 mask) ============
 *
 * 旧语义: value 存在即全拦 (任何 open 都拒)。
 * 新语义: value 是按位掩码, 各 LSM hook 按操作类型取对应位判定。
 * Rust 策略编译器把 file_rules[].deny 翻译成掩码;
 * 旧字段 file_blacklist 一律展开为 DENY_ALL (向后兼容)。
 */
#define DENY_READ    0x1   /* file_open 只读打开 */
#define DENY_WRITE   0x2   /* file_open 写意图 + path_truncate/file_truncate +
                            * mmap 共享可写 + 受保护目录内建条目(link/symlink/mkdir) */
#define DENY_DELETE  0x4   /* path_unlink / path_rmdir / rename 源侧 */
#define DENY_RENAME  0x8   /* path_rename 双侧 + path_link 源侧 (别名操纵) */
#define DENY_ATTR    0x10  /* path_chmod / path_chown / inode_setxattr */
#define DENY_ALL     0x1f

/* open(2) flag 位 (asm-generic 值, vmlinux.h 无此宏, 本地定义) */
#define O_ACCMODE    00000003
#define O_RDONLY     00000000
#define O_CREAT      00000100
#define O_TRUNC      00001000
#define O_APPEND     00002000
#define O_PATH       010000000

/* mmap(2) prot/flag 位 (asm-generic 值, 本地定义) */
#define PROT_WRITE   0x2
#define MAP_SHARED   0x01

/* vm_area_struct.vm_flags 位 (include/linux/mm.h 值, vmlinux.h 无此宏, 本地定义) */
#define VM_SHARED    0x00000008

/* 文件事件 op (event.op, kind=1 时有意义, 0=未区分) */
#define FILE_OP_OPEN_READ   1
#define FILE_OP_OPEN_WRITE  2
#define FILE_OP_TRUNCATE    3
#define FILE_OP_UNLINK      4
#define FILE_OP_RMDIR       5
#define FILE_OP_RENAME      6
#define FILE_OP_FTRUNCATE   7
#define FILE_OP_LINK        8
#define FILE_OP_SYMLINK     9
#define FILE_OP_MKDIR       10
#define FILE_OP_CHMOD       11
#define FILE_OP_CHOWN       12
#define FILE_OP_SETXATTR    13
#define FILE_OP_MMAP_WRITE  14
#define FILE_OP_REMOVEXATTR 15
#define FILE_OP_SETACL      16
#define FILE_OP_MKNOD       17
#define FILE_OP_MPROTECT    18
#define FILE_OP_FD_READ     19
#define FILE_OP_FD_WRITE    20

/* file_permission 的 mask 位 (include/linux/fs.h)。使用本地前缀避免与
 * 不同发行版 vmlinux.h / UAPI 头中的 MAY_* 宏发生重定义。 */
#define VFS_MAY_WRITE       0x2
#define VFS_MAY_READ        0x4

/* 守护者事件 op (event.op, kind=4 时有意义, 0=未区分) */
#define GUARD_OP_KILL     1
#define GUARD_OP_PTRACE   2
#define GUARD_OP_TRACEME  3
#define GUARD_OP_BPF      4

/* ============ 运行时可热更的过滤配置 (array map) ============
 *
 * 之前这些值放在 .rodata (const volatile), 加载后不可改, 改策略必须重启。
 * 为了支持 SIGHUP 热加载, 挪到 BPF_MAP_TYPE_ARRAY (max_entries=1),
 * 用户态随时 m.Update 覆盖, BPF 端每次 hook 触发时 lookup 读最新值。
 *
 * array map 的 entry 0 创建后恒存在, lookup 不会 miss;
 * 返回值判空仅为过 verifier。
 *
 * P2 说明: enable_* 是全局开关, 不做 per-domain 开关 map ——
 * "某域不启用某维度" 用空 inner 表达 (空槽/空表 = 该维度无规则,
 * 判定走默认动作), 语义已覆盖 (设计文档 §2.7)。
 */
struct filter_config {
    /* [pidtree] cgroup scope 已停用, 保留待回溯 (原字段):
     * __u64 cgroup_id;     // 目标 cgroup id, 0 = 不过滤
     */
    __u32 enable_file;   /* 文件策略开关 */
    __u32 enable_exec;   /* 命令策略开关 */
    __u32 enable_net;    /* 网络策略开关 */
    __u32 file_mode;     /* 0=白名单(暂禁用), 1=黑名单 */
    __u32 active_bank;   /* 双 bank 热更: 0/1, 单次 ARRAY update 原子切换 */
    __u32 policy_version;/* 当前全量策略版本, 审计/状态用 */
    __u32 allow_sample_rate; /* 0=关闭 ALLOW, 1=全量, N=每 N 个取 1 个 */
    __u32 audit_file;
    __u32 audit_exec;
    __u32 audit_net;
};

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct filter_config);
} filter_config_map SEC(".maps");

static __always_inline const struct filter_config *get_cfg(void)
{
    __u32 k = 0;
    return bpf_map_lookup_elem(&filter_config_map, &k);
}

/* ============ 守护者注册表 (台账 D1/D3) ============
 *
 * key = tgid, value = 1。仅 Rust daemon 启动时写入自身 pid;
 * SIGHUP 策略热更无权碰这张表 (策略通道被攻破 ≠ 守护者缴械)。
 * 不做进程退出清理: map 不 pin, 随 daemon 生灭, 只注册 daemon 自己,
 * 无 pid 复用残留面 (ActPlane 需要 exit 清理是因为它还注册运行态
 * agent 的 pid)。 */
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 64);
    __type(key, __u32);
    __type(value, __u8);
} protected_pids SEC(".maps");

/* ============ 进程树追踪集合 (pidtree P1 + runtime scope) ============
 *
 * 照搬 AcTrail 追踪集合维护机制 (设计文档
 * docs/design/多Agent域模型-进程树作用域设计.md §2/§4.1):
 * fork 立即继承 tracked，主表写失败时 pending 为全部 hook 提供降级 scope，exit 清理；
 * 追踪根由用户态 (ctl spawn/attach) 写入。
 *
 * 作用域语义: 当前进程的 tgid 在 tracked_pids 中 = 在 scope 内 = 受策略强制。
 * PID 只携带 runtime scope 身份；scope_policies 单独完成 scope → policy group
 * slot 绑定，因此任意数量的 Session/scope 可以共享少量策略组，并可通过一次
 * map 更新让整个存量进程树原子切换策略。 */

/* 追踪集合 value: tgid -> runtime scope 身份 */
struct track_val {
    __u64 scope_id;
};
#define PID_TRACKED_CAPACITY 4096
#define SCOPE_POLICY_CAPACITY 4096
#define BASE_SLOT 0
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, PID_TRACKED_CAPACITY);
    __type(key, __u32);            /* tgid */
    __type(value, struct track_val);
} tracked_pids SEC(".maps");

/* generation: tgid -> start_boottime (防 pid 复用, 根由用户态写入, 子孙 exec 落表时写入) */
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, PID_TRACKED_CAPACITY);
    __type(key, __u32);
    __type(value, __u64);
} pid_start_times SEC(".maps");

/* /proc/<pid>/stat 的 starttime 使用 USER_HZ tick；Linux 常量 USER_HZ=100。
 * 用户态 root/seed 写入该单位，fork 路径也把 task_struct.start_boottime
 * 纳秒转换为同一单位，使每个 hook 可以真正校验 PID generation。 */
#define NS_PER_USER_TICK 10000000ULL

static __always_inline __u64 task_start_ticks(struct task_struct *task)
{
    if (!task)
        return 0;
    /* tracking key 是 TGID，用户态 generation 来自 /proc/<tgid>/stat；
     * 因此内核侧也必须读取线程组 leader。直接读取当前工作线程会因其
     * start_boottime 不同而把合法进程误判为 PID 复用，并删除整个域身份。 */
    struct task_struct *leader = BPF_CORE_READ(task, group_leader);
    if (!leader)
        return 0;
    return BPF_CORE_READ(leader, start_boottime) / NS_PER_USER_TICK;
}

static __always_inline bool generation_matches(__u32 tgid,
                                               struct task_struct *task)
{
    __u64 *expected = bpf_map_lookup_elem(&pid_start_times, &tgid);
    if (!expected)
        return false;
    return *expected == task_start_ticks(task);
}

/* 主 tracking 写失败的完整降级层：所有 hook 都从这里恢复 scope 身份。 */
struct pending_proc_op {
    __u64 scope_id;
    __u64 child_start_boottime;
};
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    /* 主表满时 pending 是完整的降级 tracking 层，而不再只保护首次 exec。
     * 与主表同容量可覆盖一次完整的并发峰值；若两层都满，stats 显式告警。 */
    __uint(max_entries, PID_TRACKED_CAPACITY);
    __type(key, __u32);
    __type(value, struct pending_proc_op);
} pending_child_proc_ops SEC(".maps");

/* runtime scope 与 policy group slot 解耦。一个 slot 可被任意多个 scope 共享；
 * rebind 只需原子替换本 map 的一个 value，无需遍历或改写 tracked PID。 */
struct scope_policy_val {
    __u32 policy_slot;
    __u32 generation;
};
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, SCOPE_POLICY_CAPACITY);
    __type(key, __u64);
    __type(value, struct scope_policy_val);
} scope_policies SEC(".maps");

struct pid_tracking_stats {
    __u64 start_update_failures;
    __u64 tracked_update_failures;
    __u64 pending_update_failures;
    __u64 pending_fallbacks;
    __u64 scope_policy_lookup_failures;
};
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct pid_tracking_stats);
} pid_track_stats SEC(".maps");

enum pid_tracking_stat_kind {
    PID_STAT_START_FAILURE = 1,
    PID_STAT_TRACKED_FAILURE = 2,
    PID_STAT_PENDING_FAILURE = 3,
    PID_STAT_PENDING_FALLBACK = 4,
    PID_STAT_SCOPE_POLICY_LOOKUP_FAILURE = 5,
};

static __always_inline void record_pid_tracking_stat(__u32 kind)
{
    __u32 key = 0;
    struct pid_tracking_stats *stats =
        bpf_map_lookup_elem(&pid_track_stats, &key);
    if (!stats)
        return;
    if (kind == PID_STAT_START_FAILURE)
        __sync_fetch_and_add(&stats->start_update_failures, 1);
    else if (kind == PID_STAT_TRACKED_FAILURE)
        __sync_fetch_and_add(&stats->tracked_update_failures, 1);
    else if (kind == PID_STAT_PENDING_FAILURE)
        __sync_fetch_and_add(&stats->pending_update_failures, 1);
    else if (kind == PID_STAT_PENDING_FALLBACK)
        __sync_fetch_and_add(&stats->pending_fallbacks, 1);
    else if (kind == PID_STAT_SCOPE_POLICY_LOOKUP_FAILURE)
        __sync_fetch_and_add(&stats->scope_policy_lookup_failures, 1);
}

struct scope_info {
    __u64 scope_id;
    __u32 policy_slot;
};

static __always_inline void resolve_scope_policy(struct scope_info *si)
{
    struct scope_policy_val *binding =
        bpf_map_lookup_elem(&scope_policies, &si->scope_id);
    if (binding) {
        si->policy_slot = binding->policy_slot;
        return;
    }

    /* scope 身份仍有效，不能把受控进程误判为未受控。缺失绑定时回退到基线
     * slot，继续执行 baseline 与 self-protection，并暴露显式诊断计数。 */
    si->policy_slot = BASE_SLOT;
    record_pid_tracking_stat(PID_STAT_SCOPE_POLICY_LOOKUP_FAILURE);
}

/* tracked 与 pending 的统一 generation-safe 作用域解析。pending 是主表容量
 * 降级层，因此所有 hook、事件归属和 fork 继承都必须走同一个入口。 */
static __always_inline bool task_scope_info(__u32 tgid,
                                            struct task_struct *task,
                                            struct scope_info *si)
{
    struct track_val *tv = bpf_map_lookup_elem(&tracked_pids, &tgid);
    if (tv) {
        if (generation_matches(tgid, task)) {
            si->scope_id = tv->scope_id;
            resolve_scope_policy(si);
            return true;
        }
        /* stale PID mapping must never transfer an old domain to a reused PID. */
        bpf_map_delete_elem(&tracked_pids, &tgid);
        bpf_map_delete_elem(&pid_start_times, &tgid);
    }

    struct pending_proc_op *op =
        bpf_map_lookup_elem(&pending_child_proc_ops, &tgid);
    if (!op)
        return false;
    if (!task || op->child_start_boottime != task_start_ticks(task)) {
        bpf_map_delete_elem(&pending_child_proc_ops, &tgid);
        return false;
    }
    si->scope_id = op->scope_id;
    resolve_scope_policy(si);
    return true;
}

static __always_inline bool current_scope_info(struct scope_info *si)
{
    __u32 tgid = bpf_get_current_pid_tgid() >> 32;
    struct task_struct *task = bpf_get_current_task_btf();
    return task_scope_info(tgid, task, si);
}

/* 只要布尔语义的调用方 (守护钩) 用这个包装 */
static __always_inline bool in_scope(void)
{
    struct scope_info si = {};
    return current_scope_info(&si);
}

/* exec 路径包装：与其他 hook 使用相同的 tracked/pending scope：
 *
 * 正常路径在 sched_process_fork 当场写 tracked_pids，fork-only 子进程也
 * 立即受控。若 tracked map 临时写入失败，fork hook 会写 pending；而
 * sys_enter_execve 和 bprm_check_security 都早于 sched_process_exec，统一入口
 * 会读取 pending，避免首次 exec 在降级路径中逃逸。
 *
 * pending 与 tracked 同效提供 scope_id；策略 slot 始终经 scope_policies 解析；sched_process_exec
 * 会在 exec 成功后再次尝试正式落表。 */
static __always_inline bool in_scope_exec(struct scope_info *si)
{
    return current_scope_info(si);
}

/* 守护钩子的 scope: [pidtree] 已切换为进程树作用域 —— actor 在追踪集 =
 * scope 内, 空集 = 不生效, 语义不变。与原 "cgroup_id==0 = 不生效" 一样
 * 不会自锁: 守护钩子没有 enable_* 开关, attach 后立即生效, 而 daemon
 * 自己不在追踪集, 策略 map 更新 (bpf() 调用) 不会被拒。
 *
 * [pidtree] cgroup scope 已停用, 保留待回溯 (原实现):
 *     return cfg->cgroup_id != 0 &&
 *            bpf_get_current_cgroup_id() == cfg->cgroup_id;
 */
static __always_inline bool guard_scope(const struct filter_config *cfg)
{
    (void)cfg;
    return in_scope();
}

/* ============ 多域策略 map-in-map (P2 核心) ============
 *
 * slot 是稳定的身份, inner map 是可换的内容:
 *   tracked_pids[tgid].scope_id ──> scope_policies[scope_id].policy_slot
 *                                ──> outer[slot] ──> inner map ──> 规则
 * 热更新只替换 outer 槽位指向的 fd；scope rebind 只替换 scope_policies value。
 *
 * 判定路径: 基线 slot 0 与域 slot 并查 (掩码取并集, 版本取命中者);
 * 域规则后查, 两侧都命中时事件版本取域侧的 (更具体)。
 *
 * ARRAY_OF_MAPS 说明:
 *   - outer lookup 返回 inner map 指针, 直接作为第二次 lookup 的第一个
 *     参数, 是 verifier 支持的标准模式;
 *   - 多个槽位允许引用同一个 inner map fd —— 策略组共享不需要任何
 *     额外内核结构, Rust loader 把同一个 fd 写进多个 slot;
 *   - outer lookup 失败 (空槽, 如未配 __base__ 的 slot 0) 与 inner miss
 *     (无匹配规则) 是两个语义, 分别走各自的默认动作;
 *   - inner 指针仅在本次程序执行内有效, 每次 hook 重新查, 开销纳秒级;
 *   - 内核 bpf_map_meta_equal 要求 inner 的 type/key/value/flags/
 *     max_entries 与模板完全一致, 所以用户态创建 inner 一律用模板原样
 *     属性 (max_entries 不按需缩小; HASH 开 NO_PREALLOC 省内存)。
 */
#define POLICY_BANK_SIZE 64   /* 每 bank: slot 0 基线, 1..63 域 */
#define MAX_POLICY_SLOTS 128  /* bank 0: 0..63; bank 1: 64..127 */

static __always_inline __u32 physical_slot(const struct filter_config *cfg,
                                           __u32 logical_slot)
{
    return (cfg->active_bank & 1) * POLICY_BANK_SIZE +
           (logical_slot & (POLICY_BANK_SIZE - 1));
}

/* ---- inner 模板 (ARRAY_OF_MAPS 要求显式声明 value 类型) ---- */
struct ino_hash_inner {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 256);
    __uint(map_flags, BPF_F_NO_PREALLOC);   /* 条目少时省内存 */
    __type(key, struct ino_key);
    __type(value, struct ino_rule_val);
};
struct str_hash_inner {          /* file_str 维 */
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 256);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, struct path_key);
    __type(value, struct str_rule_val);
};
struct cmd_hash_inner {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 256);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, struct cmd_key);
    __type(value, struct str_rule_val);
};
struct arg_hash_inner {
    __uint(type, BPF_MAP_TYPE_HASH);
    /* 每条存在的参数规则同时存 path 与 inode 两种身份，保持最多 256 条
     * 用户规则时不会因双编码提前触顶。 */
    __uint(max_entries, 512);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, struct arg_key);
    __type(value, struct str_rule_val);
};
struct net_lpm_inner {
    __uint(type, BPF_MAP_TYPE_LPM_TRIE);
    __uint(max_entries, 1024);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, struct net_lpm_key);
    __type(value, struct net_rule_val);
};
struct net_port_lpm_inner {
    __uint(type, BPF_MAP_TYPE_LPM_TRIE);
    __uint(max_entries, 1024);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, struct net_port_lpm_key);
    __type(value, struct net_rule_val);
};
struct net6_lpm_inner {
    __uint(type, BPF_MAP_TYPE_LPM_TRIE);
    __uint(max_entries, 1024);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, struct net6_lpm_key);
    __type(value, struct net_rule_val);
};
struct net6_port_lpm_inner {
    __uint(type, BPF_MAP_TYPE_LPM_TRIE);
    __uint(max_entries, 1024);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, struct net6_port_lpm_key);
    __type(value, struct net_rule_val);
};

/* ---- outer: 每策略维度一张, key = slot ---- */
#define DECLARE_OUTER(name, inner_type)                          \
    struct {                                                     \
        __uint(type, BPF_MAP_TYPE_ARRAY_OF_MAPS);                \
        __uint(max_entries, MAX_POLICY_SLOTS);                   \
        __type(key, __u32);                                      \
        __array(values, struct inner_type);                      \
    } name SEC(".maps")

DECLARE_OUTER(dom_file_str,  str_hash_inner);
DECLARE_OUTER(dom_file_ino,  ino_hash_inner);
DECLARE_OUTER(dom_dir_ino,   ino_hash_inner);
DECLARE_OUTER(dom_cmd,       cmd_hash_inner);
DECLARE_OUTER(dom_cmd_ino,   ino_hash_inner);
DECLARE_OUTER(dom_arg,       arg_hash_inner);
DECLARE_OUTER(dom_net,       net_lpm_inner);
DECLARE_OUTER(dom_net_port,  net_port_lpm_inner);
DECLARE_OUTER(dom_net6,      net6_lpm_inner);
DECLARE_OUTER(dom_net6_port, net6_port_lpm_inner);

/* 强制 BTF 完整发射 inner 模板的 key/value 类型: 模板只经 typeof 引用,
 * clang 对未完整使用的结构体只留 FWD 前向声明, cilium 解析 inner map
 * 定义时会报 "type is unsized"。同 _unused_event 的用法。 */
struct path_key *_unused_path_key __attribute__((unused));
struct cmd_key *_unused_cmd_key __attribute__((unused));
struct ino_key *_unused_ino_key __attribute__((unused));
struct ino_rule_val *_unused_ino_rule_val __attribute__((unused));
struct str_rule_val *_unused_str_rule_val __attribute__((unused));
struct net_lpm_key *_unused_net_lpm_key __attribute__((unused));
struct net_port_lpm_key *_unused_net_port_lpm_key __attribute__((unused));
struct net6_lpm_key *_unused_net6_lpm_key __attribute__((unused));
struct net6_port_lpm_key *_unused_net6_port_lpm_key __attribute__((unused));
struct net_rule_val *_unused_net_rule_val __attribute__((unused));

/* ---- slot 元数据表: 与 outer 按下标对齐, 事件版本兜底 + 审计 ---- */
struct slot_meta {
    __u32 version;       /* 当前槽位生效的策略版本 */
    __u32 policy_group;  /* 绑定的策略组 id */
    __u64 switch_ts;     /* 换槽时间戳 (控制面写入, UnixNano), 审计用 */
};
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_POLICY_SLOTS);
    __type(key, __u32);
    __type(value, struct slot_meta);
} slot_meta_map SEC(".maps");

/* 白名单相关变量 - 暂时不用, 保留占位
const volatile char   filter_allow_dir[DIR_MAX_LEN];
const volatile __u32  filter_allow_dir_len  = 0;

struct cmd_entry { char s[CMD_MAX_LEN]; };
const volatile __u32  filter_deny_cmds[FIXED_MAX] = { {0} };
const volatile __u32  filter_deny_cmds_n = 0;

struct path_entry { char s[PATH_STR_MAX]; };
const volatile struct path_entry filter_allow_paths[PATH_LENS_MAX] = { {0} };
const volatile __u32 filter_allow_paths_n = 0;
*/

#if 0  /* 白名单路径 - 暂时禁用 */
/* 黑名单单一长路径 */
const volatile char   filter_deny_dir[DIR_MAX_LEN];
const volatile __u32  filter_deny_dir_len  = 0;
#endif

#if 0
/* 内置放通前缀 —— 编译期常量, 永不变化。
 *
 * 用途: cgroup 内的进程跑 bash / sleep / cat / 动态链接器 / locale 查表
 * 这些操作都会触发 file_open。-allow-dir 和 -allow-paths 用户给的
 * 是"业务路径" (e.g. /opt/agent/workspace), 不会包括 /usr/lib、
 * /usr/share/locale、/dev 之类。如果不在内置放通里, cgroup 内
 * 任何基础命令都跑不起来。
 *
 * 注意: 每条最长 PATH_STR_MAX-1 = 15 字符, 用完要换 "/usr/lib64" ->
 * "/usr/lib" 这种缩写。
 *
 * 顺序无所谓: BPF 按数组线性扫, 命中即返。
 */
static const char builtin_allow_prefixes[BUILTIN_ALLOW_N][PATH_STR_MAX] = {
    "/lib",
    "/lib64",
    "/usr/lib",
    "/usr/lib64",
    "/usr/share/loc",
    "/usr/share/ter",
    "/etc/ld.so.",
    "/dev/",
    "/usr/bin/bash",
    "/usr/bin/sh",
    "/usr/bin/sleep",
    "/usr/bin/cat",
    "/usr/bin/ls",
    "/usr/bin/echo",
    "/usr/bin/python",
    "/usr/bin/cp",
    "/usr/bin/mv",
    "/usr/bin/rm",
    "/usr/bin/mkdir",
    "/usr/bin/head",
    "/usr/bin/tail",
    "/usr/bin/grep",
    "/usr/bin/awk",
    "/usr/bin/sed",
    "/usr/bin/sort",
    "/usr/bin/wc",
    "/usr/bin/uname",
    "/usr/bin/which",
    "/usr/bin/curl",
    "/usr/bin/wget",
    "/usr/bin/nc",
    "/proc/",
    "/sys/fs/cgrou",  /* /sys/fs/cgroup + 子目录 */
};
#endif

/* ============ ringbuf event ============ */
struct event {
    __u64 cgroup_id;
    __u64 ts_ns;
    __u32 pid, tgid, kind;
    __u32 allowed;
    __u32 op;            /* kind=1 时为 FILE_OP_*; 其余 kind 恒 0 */
    __u32 policy_version; /* P2: 判定依据的策略版本 (slot_meta, 兜底) */
    __u32 rule_version;   /* P2: 命中规则自身的版本, 未命中 = 0 */
    __u64 domain_id;      /* Web 控制台: 事件域归属 (tracked_pids; 未追踪 = 0) */
    char  comm[16];
    char  detail[160];
    /* exec 参数 token (kind=2 参数级命中时有值): 内核不拼字符串
     * (变长栈操作反复踩 verifier), 原样上报, Rust audit 拼接展示 */
    char  args[ARG_TOKEN_N][ARG_TOKEN_LEN];
};
struct event *_unused_event __attribute__((unused));

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 256 * 1024);
} events SEC(".maps");

/* ringbuf 满时 reserve 失败不会产生 sample，必须用独立 map 记录，否则
 * 用户态只能看到 C shim 暂存队列丢弃，无法知道真正的内核侧缺口。 */
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u64);
} event_drops SEC(".maps");

/* ALLOW 采样只影响低优先级正常事件。per-CPU 计数避免所有 CPU 在高频
 * 文件/网络热路径上争用同一个原子变量；policy_version 变化时每个 CPU
 * 首次使用会自行清零，使热更新后的采样周期有明确边界。 */
struct allow_sample_counter {
    __u64 count;
    __u32 policy_version;
    __u32 pad;
};
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct allow_sample_counter);
} allow_sample_counters SEC(".maps");

static __always_inline void count_event_drop(void)
{
    __u32 key = 0;
    __u64 *dropped = bpf_map_lookup_elem(&event_drops, &key);
    if (dropped)
        __sync_fetch_and_add(dropped, 1);
}

static __always_inline bool should_report_allow(void)
{
    const struct filter_config *cfg = get_cfg();
    if (!cfg || cfg->allow_sample_rate == 0)
        return false;
    if (cfg->allow_sample_rate == 1)
        return true;

    __u32 key = 0;
    struct allow_sample_counter *counter =
        bpf_map_lookup_elem(&allow_sample_counters, &key);
    if (!counter)
        return false;
    if (counter->policy_version != cfg->policy_version) {
        counter->count = 0;
        counter->policy_version = cfg->policy_version;
    }
    counter->count++;
    return counter->count % cfg->allow_sample_rate == 0;
}

/* argv 暂存: sys_enter_execve 写入, bprm_check_security 读出后即删。
 * LRU 自动驱逐残留项 (exec 中途失败走不到 LSM hook 的情况) */
struct {
    __uint(type, BPF_MAP_TYPE_LRU_HASH);
    __uint(max_entries, 1024);
    __type(key, __u64);
    __type(value, struct exec_args);
} exec_args_map SEC(".maps");

/* is_args_in_blacklist 的 scratch key 缓冲。
 * BPF 栈只有 512B, arg_key(128B) 内联进 bprm hook 会超限,
 * 按 clang 建议放 per-cpu array (单 CPU 独占, 无需锁)。
 * 与域无关 (查询 key 的构造不碰策略表), 全组共享一个。 */
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct arg_key);
} arg_key_scratch SEC(".maps");

#if 0
/* ============ 公共: 单条 allow_paths[i] 与 path 前缀比较 ============
 *
 * j 是循环变量 (编译期常量 0..15), 运行时 want/path[j] 比较。
 * 命中/失败不能 early-return (会破 unroll), 用 fail 标志记录,
 * 循环结束后统一判断。
 */
static __always_inline bool path_matches_allow_entry(int idx, const char *path)
{
    /* matched: -1 = 待定, 0 = 失败, 1 = 命中 */
    int matched = -1;
    #pragma unroll
    for (int j = 0; j < 16; j++) {
        char want = filter_allow_paths[idx].s[j];
        if (matched == 0) continue;          /* 已失败, 不再更新 */
        if (want == '\0') {
            matched = 1;                      /* 规则读完, 算匹配 */
        } else if (path[j] != want) {
            matched = 0;                      /* 不匹配 */
        }
    }
    return matched == 1;
}

/* ============ 公共: 单条 deny_paths[i] 与 path 前缀比较 ============
 *
 * 与 path_matches_allow_entry 结构相同, 但从 filter_deny_paths 读取。
 * 分开两个函数是为了避免在循环中动态选择数组(verifier 会拒)。
 */
static __always_inline bool path_matches_deny_entry(int idx, const char *path)
{
    /* matched: -1 = 待定, 0 = 失败, 1 = 命中 */
    int matched = -1;
    #pragma unroll
    for (int j = 0; j < 16; j++) {
        char want = filter_deny_paths[idx].s[j];
        if (matched == 0) continue;
        if (want == '\0') {
            matched = 1;                      /* 规则读完, 算匹配 */
        } else if (path[j] != want) {
            matched = 0;                      /* 不匹配 */
        }
    }
    return matched == 1;
}

/* ============ 公共: 内置前缀与 path 前缀比较 ============
 * builtin_allow_prefixes[][] 是编译期 const, 数组长度 BUILTIN_ALLOW_N
 * 是编译期常量, #pragma unroll 可展开。
 */
static __always_inline bool builtin_matches(int idx, const char *path)
{
    int matched = -1;
    #pragma unroll
    for (int j = 0; j < 16; j++) {
        char want = builtin_allow_prefixes[idx][j];
        if (matched == 0) continue;
        if (want == '\0') {
            matched = 1;                      /* 规则读完, 算匹配 */
        } else if (path[j] != want) {
            matched = 0;                      /* 不匹配 */
        }
    }
    return matched == 1;
}

/* ============ 公共: 多目录前缀匹配 (用户 + 内置) ============ */
static __always_inline bool has_allowed_path(const char *path)
{
    int matched = 0;
    /* 先查用户白名单 (filter_allow_paths[]) */
    #pragma unroll
    for (int i = 0; i < 8; i++) {
        if (matched) continue;
        if (i < filter_allow_paths_n && path_matches_allow_entry(i, path))
            matched = 1;
    }
    /* 再查内置放通 (builtin_allow_prefixes[][]) */
    #pragma unroll
    for (int i = 0; i < 33; i++) {
        if (matched) continue;
        if (builtin_matches(i, path))
            matched = 1;
    }
    return matched;
}

/* ============ 公共: 检查路径是否在黑名单中 ============
 *
 * 用于黑名单模式：只有命中黑名单的路径才被拦截。
 *
 * 注意：内置白名单 (builtin_allow_prefixes) 不适用于黑名单模式，
 * 因为黑名单默认允许所有路径，除非显式命中黑名单规则。
 */
static __always_inline bool has_denied_path(const char *path)
{
    int matched = 0;
    /* 查用户黑名单 (filter_deny_paths[]) */
    #pragma unroll
    for (int i = 0; i < 8; i++) {
        if (matched) continue;
        if (i < filter_deny_paths_n && path_matches_deny_entry(i, path))
            matched = 1;
    }
    return matched;
}

/* ============ 公共: 单 filter_deny_dir 前缀匹配 ============ */
static __always_inline bool has_denied_dir(const char *path)
{
    int len = filter_deny_dir_len;
    if (len <= 0 || len >= 32)
        return false;
    int matched = 1;
    #pragma unroll
    for (int j = 0; j < 32; j++) {
        if (j < len && matched && path[j] != filter_deny_dir[j])
            matched = 0;
    }
    return matched;
}

/* ============ 公共: 单 filter_allow_dir 前缀匹配 ============ */
static __always_inline bool has_allowed_dir(const char *path)
{
    int len = filter_allow_dir_len;
    if (len <= 0 || len >= 32)
        return false;
    int matched = 1;
    #pragma unroll
    for (int j = 0; j < 32; j++) {
        /* 用条件分支代替 break, 让 clang 能 unroll */
        if (j < len && matched && path[j] != filter_allow_dir[j])
            matched = 0;
    }
    return matched;
}

/* ============ 路径前缀匹配总入口 ============ */
static __always_inline bool path_has_allowed_prefix(const char *path)
{
    if (has_allowed_path(path))
        return true;
    if (filter_allow_dir_len > 0 && filter_allow_dir_len < 32
        && has_allowed_dir(path))
        return true;
    return false;
}
#endif /* 白名单相关函数 - 暂时禁用 */

/* ============ 公共: detail 拷到 ringbuf event ============
 *
 * 关键设计: 不在 BPF 程序里做字节循环复制。改用 bpf_probe_read_kernel_str
 * (helper, verifier-trusted, 无循环展开) 一次性把 src 拷到 dst,
 * 然后显式把 dst[copy_max..159] 清零。
 *
 * verifier 看到的是:
 *   1. helper 调用 (不展开)
 *   2. 一个简单 for 循环写 '\0' 到 dst[i], 循环边界 159 (编译期常量),
 *      body 只有 1 条 store 指令
 *
 * 实测这种写法 single program 几百条指令, 三个 program 总和远低于 100 万。
 */
static __always_inline void copy_to_detail(char *dst, int dst_max, const char *src)
{
    /* helper: 把 src (kernel ptr, 含 NUL 终止) 截断拷到 dst, 返回总字节数 (含 NUL) */
    long n = bpf_probe_read_kernel_str(dst, dst_max, src);
    /* n > 0 时 NUL 已在 dst[n-1], n < 0 表示读失败 (dst 全 0)
     * n >= dst_max 时 src 被截断, 但 dst[dst_max-1] 仍是 NUL (helper 行为)
     * 不论如何, 后面清零 dst[dst_max..158] —— 不用 #pragma unroll,
     * 普通有界循环 ~3 条循环头 + 1 条 store */
    (void)n;
    for (int i = dst_max; i < 159; i++) {
        dst[i] = '\0';
    }
}

/* ============ 策略查询: 基线 + 域两级 inner 并查 ============
 *
 * 统一模式: 每个 hook 对每张 outer 只查一次 (基线 slot + 域 slot),
 * 拿到 inner 指针后传入各判定函数复用。base 先查 dom 后查,
 * 掩码取并集; rule_ver 取命中者版本, 两侧都命中时取域侧 (更具体)。
 * NULL inner (空槽, 主要是未配 __base__ 的 slot 0) 跳过。
 */

/* 从 inode 构造查询 key (普通内存读, 任何钩子可用) */
static __always_inline void ino_key_from_inode(struct inode *inode,
                                               struct ino_key *k)
{
    k->ino = BPF_CORE_READ(inode, i_ino);
    k->dev = BPF_CORE_READ(inode, i_sb, s_dev);
}

/* ino 类规则并查: 基线 inner + 域 inner */
static __always_inline __u8 ino_mask_lookup(struct bpf_map *base,
                                            struct bpf_map *dom,
                                            const struct ino_key *k,
                                            __u32 *rule_ver)
{
    __u8 mask = 0;
    if (base) {
        struct ino_rule_val *rv = bpf_map_lookup_elem(base, k);
        if (rv) {
            if (rv->action == 0)
                mask |= rv->mask;
            *rule_ver = rv->version;
            if (rv->audit)
                *rule_ver |= 0x80000000u;
        }
    }
    if (dom) {
        struct ino_rule_val *rv = bpf_map_lookup_elem(dom, k);
        if (rv) {
            if (rv->action == 0)
                mask |= rv->mask;
            *rule_ver = rv->version;
            if (rv->audit)
                *rule_ver |= 0x80000000u;
        }
    }
    return mask;
}

/* 精确受害者查询: 文件自身 / 受保护目录自身 */
static __always_inline __u8 file_ino_mask(struct inode *inode,
                                          struct bpf_map *base,
                                          struct bpf_map *dom,
                                          __u32 *rule_ver)
{
    if (!inode)
        return 0;
    struct ino_key k = {};
    ino_key_from_inode(inode, &k);
    return ino_mask_lookup(base, dom, &k, rule_ver);
}

/* 祖先走链预算。dentry 指针在 bounded-loop 回边后会被当前 6.6 verifier
 * 降级为 scalar，并造成百万指令状态爆炸；因此保留静态展开，但把预算从 8
 * 提升到 32。极端深度耗尽预算时 fail closed，不再静默逃逸。 */
#define DENTRY_WALK_MAX  32

/* 祖先走链查目录集合: 从 victim 的父目录开始, 每级在基线+域 inner 并查
 * 一次, 命中即并入 mask。fs 根 (d_parent 指向自己) 停止。
 * 普通内存读 + map 查询, 无字符串操作, verifier 友好。 */
static __always_inline __u8 dir_ancestor_mask(struct dentry *dentry,
                                              struct bpf_map *base,
                                              struct bpf_map *dom,
                                              __u32 *rule_ver)
{
    __u8 mask = 0;
    bool stop = false;
    bool invalid = false;
    struct dentry *cur = dentry;
#pragma unroll
    for (int i = 0; i < DENTRY_WALK_MAX; i++) {
        if (!stop && cur) {
            struct dentry *parent = BPF_CORE_READ(cur, d_parent);
            if (!parent) {
                stop = true;
                invalid = true;
            } else if (parent == cur) {
                stop = true;
            } else {
                struct inode *pi = BPF_CORE_READ(parent, d_inode);
                if (pi) {
                    struct ino_key k = {};
                    ino_key_from_inode(pi, &k);
                    mask |= ino_mask_lookup(base, dom, &k, rule_ver);
                }
                cur = parent;
            }
        }
    }
    /* 未停止表示仍未到根；invalid 表示祖先读取异常。两者都无法证明不存在
     * 更高层受保护目录，因此拒绝全部文件操作。 */
    return (!stop || invalid) ? (mask | DENY_ALL) : mask;
}

/* 文件钩子统一前奏: scope 判定 + file_ino/dir_ino 两维 outer 各查
 * 基线/域一次, inner 指针传出复用; 带回域 slot 与兜底策略版本。
 * 返回 false = 不在 scope / 两维都无表 (空槽), 调用方直接放行。 */
struct file_policy {
    struct bpf_map *base_fino;
    struct bpf_map *dom_fino;
    struct bpf_map *base_dino;
    struct bpf_map *dom_dino;
    __u32 slot;
    __u32 policy_ver;
};

static __always_inline bool file_policy_setup(const struct filter_config *cfg,
                                              struct file_policy *fp)
{
    struct scope_info si = {};
    if (!current_scope_info(&si))
        return false;
    __u32 base = physical_slot(cfg, BASE_SLOT);
    __u32 domain = physical_slot(cfg, si.policy_slot);
    fp->base_fino = bpf_map_lookup_elem(&dom_file_ino, &base);
    fp->dom_fino  = bpf_map_lookup_elem(&dom_file_ino, &domain);
    fp->base_dino = bpf_map_lookup_elem(&dom_dir_ino, &base);
    fp->dom_dino  = bpf_map_lookup_elem(&dom_dir_ino, &domain);
    /* 四个 inner 指针逐一判空聚成标量, 绝不合并写 !a && !b && !c && !d:
     * clang 会 if-convert 成 (a|b|c|d)==0, 即对指针做位或, verifier
     * 明令禁止 "pointer |= pointer" 直接拒载 (P2 实测教训, 见设计文档附B)。 */
    __u32 have = 0;
    if (fp->base_fino)
        have |= 1;
    if (fp->dom_fino)
        have |= 2;
    if (fp->base_dino)
        have |= 4;
    if (fp->dom_dino)
        have |= 8;
    if (!have)
        return false;   /* 两维全空槽 = 无文件策略 */
    fp->slot = domain;
    struct slot_meta *sm = bpf_map_lookup_elem(&slot_meta_map, &fp->slot);
    fp->policy_ver = sm ? sm->version : 0;
    return true;
}

/* 统一判定入口: 受害者自身 (file_ino 维) | 祖先目录 (dir_ino 维)。
 * dentry->d_inode 允许为 NULL (rename 目标不存在时), 自身查询跳过,
 * 祖先走链照常 —— 在受保护目录里新建文件也能被目录规则拦住。 */
static __always_inline __u8 victim_mask(struct dentry *dentry,
                                        const struct file_policy *fp,
                                        __u32 *rule_ver)
{
    if (!dentry)
        return 0;
    __u8 m = file_ino_mask(BPF_CORE_READ(dentry, d_inode),
                           fp->base_fino, fp->dom_fino, rule_ver);
    return m | dir_ancestor_mask(dentry, fp->base_dino, fp->dom_dino, rule_ver);
}

/* 字符串兜底查询 (file_str 维): 完整路径 -> DENY_* 掩码 (未命中返回 0)。
 * 策略路径在加载时不存在 (stat 失败) 的规则只能按路径拦 open。 */
static __always_inline __u8 file_deny_mask(const char *path,
                                           struct bpf_map *base,
                                           struct bpf_map *dom,
                                           __u32 *rule_ver)
{
    struct path_key key = {};
    /* 拷贝最多 63 字节 (留 1 字节给 NUL) */
    bpf_probe_read_kernel_str(key.s, sizeof(key.s), path);
    struct str_rule_val *rv;
    __u8 mask = 0;
    if (base) {
        rv = bpf_map_lookup_elem(base, &key);
        if (rv) {
            if (rv->action == 0)
                mask |= rv->mask;
            *rule_ver = rv->version;
            if (rv->audit)
                *rule_ver |= 0x80000000u;
        }
    }
    if (dom) {
        rv = bpf_map_lookup_elem(dom, &key);
        if (rv) {
            if (rv->action == 0)
                mask |= rv->mask;
            *rule_ver = rv->version;
            if (rv->audit)
                *rule_ver |= 0x80000000u;
        }
    }
    return mask;
}

/* 命令字符串兜底并查: 只用于策略加载时 stat 失败的路径。 */
static __always_inline bool is_cmd_in_blacklist(const char *cmd,
                                                struct bpf_map *base,
                                                struct bpf_map *dom,
                                                __u32 *rule_ver)
{
    struct cmd_key key;
    __builtin_memset(&key, 0, sizeof(key));
    bpf_probe_read_kernel_str(key.s, sizeof(key.s), cmd);
    struct str_rule_val *rv;
    if (base) {
        rv = bpf_map_lookup_elem(base, &key);
        if (rv) {
            *rule_ver = rv->version;
            if (rv->audit)
                *rule_ver |= 0x80000000u;
            return rv->action == 0;
        }
    }
    if (dom) {
        rv = bpf_map_lookup_elem(dom, &key);
        if (rv) {
            *rule_ver = rv->version;
            if (rv->audit)
                *rule_ver |= 0x80000000u;
            return rv->action == 0;
        }
    }
    return false;
}

/* 命令主判定: bprm->file 是内核已经打开的实际可执行对象，因此 inode
 * 不受 execveat 的 dirfd/AT_EMPTY_PATH 形式，也不受软硬链接别名影响。 */
static __always_inline bool is_cmd_inode_in_blacklist(struct file *file,
                                                      struct bpf_map *base,
                                                      struct bpf_map *dom,
                                                      __u32 *rule_ver)
{
    if (!file)
        return false;
    struct inode *inode = BPF_CORE_READ(file, f_inode);
    if (!inode)
        return false;
    struct ino_key key = {};
    ino_key_from_inode(inode, &key);
    return ino_mask_lookup(base, dom, &key, rule_ver) != 0;
}

/* 对一个已构造的 argument key 做 4→1 token 前缀回退。 */
static __always_inline bool arg_key_in_blacklist(struct arg_key *k,
                                                 struct bpf_map *base,
                                                 struct bpf_map *dom,
                                                 __u32 *rule_ver)
{
    struct str_rule_val *rv;

#define ARG_HIT()                                                     \
    ({                                                                \
        bool hit = false;                                             \
        if (base) {                                                   \
            rv = bpf_map_lookup_elem(base, k);                        \
            if (rv && rv->action == 0) { *rule_ver = rv->version | (rv->audit ? 0x80000000u : 0); hit = true; }          \
        }                                                             \
        if (!hit) {                                                   \
            if (dom) {                                                \
                rv = bpf_map_lookup_elem(dom, k);                     \
                if (rv && rv->action == 0) { *rule_ver = rv->version | (rv->audit ? 0x80000000u : 0); hit = true; }      \
            }                                                         \
        }                                                             \
        hit;                                                          \
    })

    if (ARG_HIT())
        return true;
    __builtin_memset(k->tok[3], 0, ARG_TOKEN_LEN);
    if (ARG_HIT())
        return true;
    __builtin_memset(k->tok[2], 0, ARG_TOKEN_LEN);
    if (ARG_HIT())
        return true;
    __builtin_memset(k->tok[1], 0, ARG_TOKEN_LEN);
    if (ARG_HIT())
        return true;
    return false;
#undef ARG_HIT
}

/* 参数级黑名单并查: token 逐级回退匹配。
 * 主身份是 bprm->file 的 (dev,ino)，tok[0] 编码 marker + inode key；
 * 策略 stat 失败时才回退到 bprm->filename 字符串。tok[1..3] 来自
 * sys_enter_execve/sys_enter_execveat 暂存的真实参数。
 * 规则 "/usr/bin/git push" 只有前 2 槽 —— 把 key 的尾槽逐级清零再查,
 * 实现前缀语义: 写 "/usr/bin/git push" 即禁所有 "git push *"。
 * 全部定长 memcpy/memset, 无变长操作。
 * 每一级回退都是 基线→域 并查; outer 由调用方各查一次传入复用。 */
static __always_inline bool is_args_in_blacklist(const struct exec_args *ea,
                                                 struct file *file,
                                                 const char *filename,
                                                 struct bpf_map *base,
                                                 struct bpf_map *dom,
                                                 __u32 *rule_ver)
{
    __u32 z = 0;
    struct arg_key *k = bpf_map_lookup_elem(&arg_key_scratch, &z);
    if (!k)
        return false;

    if (file) {
        struct inode *inode = BPF_CORE_READ(file, f_inode);
        if (inode) {
            struct ino_key ik = {};
            ino_key_from_inode(inode, &ik);
            __builtin_memcpy(k, ea->tok, sizeof(*k));
            __builtin_memset(k->tok[0], 0, ARG_TOKEN_LEN);
            k->tok[0][0] = ARG_INODE_MARKER;
            __builtin_memcpy(k->tok[0] + ARG_INODE_OFFSET, &ik, sizeof(ik));
            if (arg_key_in_blacklist(k, base, dom, rule_ver))
                return true;
        }
    }

    if (filename) {
        __builtin_memcpy(k, ea->tok, sizeof(*k));
        bpf_probe_read_kernel_str(k->tok[0], ARG_TOKEN_LEN, filename);
        if (arg_key_in_blacklist(k, base, dom, rule_ver))
            return true;
    }
    return false;
}

/* 把一个 0-255 的八位组定宽写成 3 个 ASCII 字符 (带前导零)。
 * 用于把 daddr 格式化成 "008.008.004.004" 放进 event detail。
 *
 * 为什么定宽: 变长写法 (1-3 字符, dst 偏移运行时变化) 需要 verifier
 * 追踪可变栈偏移, 容易被拒导致整个 socket_connect 程序加载失败;
 * 定宽全部常量偏移, verifier 一定能过。 */
static __always_inline void fmt_octet3(char *dst, __u32 v)
{
    dst[0] = '0' + v / 100;
    dst[1] = '0' + (v / 10) % 10;
    dst[2] = '0' + v % 10;
}

/* 把 0-65535 的端口定宽写成 5 个 ASCII 字符 (带前导零)。
 * 定宽原因同 fmt_octet3: 全常量偏移, verifier 一定能过。 */
static __always_inline void fmt_port5(char *dst, __u16 v)
{
    dst[0] = '0' + v / 10000;
    dst[1] = '0' + (v / 1000) % 10;
    dst[2] = '0' + (v / 100) % 10;
    dst[3] = '0' + (v / 10) % 10;
    dst[4] = '0' + v % 10;
}

/* IPv6 审计使用 32 个固定十六进制字符，不在 BPF 中实现 RFC 5952 压缩。
 * 固定偏移便于 verifier 证明所有写入都在 detail 缓冲区内。 */
static __always_inline void fmt_ipv6_hex(char *dst, const __u8 *addr)
{
#pragma unroll
    for (int i = 0; i < 16; i++) {
        __u8 high = addr[i] >> 4;
        __u8 low = addr[i] & 0xf;
        dst[i * 2] = high < 10 ? '0' + high : 'a' + high - 10;
        dst[i * 2 + 1] = low < 10 ? '0' + low : 'a' + low - 10;
    }
}

/* 定宽十进制 2 位 / 7 位 (全常量偏移, verifier 理由同 fmt_port5)。
 * 守护事件 detail 里展示 sig / tgid 用。 */
static __always_inline void fmt_dec2(char *dst, __u32 v)
{
    dst[0] = '0' + (v / 10) % 10;
    dst[1] = '0' + v % 10;
}
static __always_inline void fmt_dec7(char *dst, __u32 v)
{
    dst[0] = '0' + (v / 1000000) % 10;
    dst[1] = '0' + (v / 100000) % 10;
    dst[2] = '0' + (v / 10000) % 10;
    dst[3] = '0' + (v / 1000) % 10;
    dst[4] = '0' + (v / 100) % 10;
    dst[5] = '0' + (v / 10) % 10;
    dst[6] = '0' + v % 10;
}

/* ============ 公共: 上报 (P2 起携带策略版本) ============
 *
 * rule_ver 的最高位仅在 BPF 内部作为逐规则 audit 标志使用。写入 ringbuf
 * 前会清除该位，因此用户态仍收到原始 u32 规则版本；命中 allow+audit 时
 * 即使全局 ALLOW 采样关闭，也会强制生成事件。
 */
static __always_inline void report(__u32 kind, __u32 op, __u32 allowed,
                                   const char *detail,
                                   __u32 policy_ver, __u32 rule_ver)
{
    bool rule_audit = (rule_ver & 0x80000000u) != 0;
    rule_ver &= 0x7fffffffu;
    if (allowed) {
        const struct filter_config *cfg = get_cfg();
        bool audit_enabled = cfg && ((kind == 1 && cfg->audit_file) ||
                                     (kind == 2 && cfg->audit_exec) ||
                                     (kind == 3 && cfg->audit_net) ||
                                     kind == 4);
        if (!rule_audit && (!audit_enabled || !should_report_allow()))
            return;
    }

    struct event *e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
    if (!e) {
        count_event_drop();
        return;
    }

    __u64 id = bpf_get_current_pid_tgid();
    e->tgid = id >> 32;
    e->pid  = (__u32)id;
    e->ts_ns = bpf_ktime_get_ns();
    e->cgroup_id = bpf_get_current_cgroup_id();
    e->kind = kind;
    e->op = op;
    e->allowed = allowed;
    e->policy_version = policy_ver;
    e->rule_version = rule_ver;
    /* 与判定复用统一 scope，容量降级到 pending 时事件仍保留域归属。 */
    struct scope_info si = {};
    e->domain_id = current_scope_info(&si) ? si.scope_id : 0;
    bpf_get_current_comm(e->comm, sizeof(e->comm));

    /* ringbuf_reserve 不清零内存: args 只对 EXEC 参数级事件有意义,
     * 其余事件必须显式清零, 否则用户态会把槽位里上一个事件的
     * 残留字节当 args 打印出来 (脏数据) */
    __builtin_memset(e->args, 0, sizeof(e->args));

    copy_to_detail(e->detail, 159, detail);
    e->detail[159] = '\0';

    bpf_ringbuf_submit(e, 0);
}

/* 固定宽度 hex (16 字符, 全常量偏移) —— 事件 detail 里展示 ino 用 */
static __always_inline void fmt_hex16(char *dst, __u64 v)
{
#pragma unroll
    for (int i = 0; i < 16; i++) {
        __u8 nib = (v >> ((15 - i) * 4)) & 0xf;
        dst[i] = nib < 10 ? '0' + nib : 'a' + nib - 10;
    }
}

/* path_* 钩子的事件上报: detail = "ino=<hex> <受害者名>"。
 * 不写完整路径 (非 sleepable 里拿不到), ino + 名字 + op 足够定位,
 * ino 可与 ls -i / stat 对照。 */
static __always_inline void report_path_op(__u32 op, __u32 allowed,
                                           struct dentry *dentry,
                                           __u32 policy_ver, __u32 rule_ver)
{
    char detail[96] = {};
    __builtin_memcpy(detail, "ino=", 4);
    struct inode *inode = BPF_CORE_READ(dentry, d_inode);
    __u64 ino = inode ? BPF_CORE_READ(inode, i_ino) : 0;
    fmt_hex16(detail + 4, ino);
    detail[20] = ' ';
    const unsigned char *nm = BPF_CORE_READ(dentry, d_name.name);
    if (nm)
        bpf_probe_read_kernel_str(detail + 21, sizeof(detail) - 21, nm);
    report(1, op, allowed, detail, policy_ver, rule_ver);
}

/* ===================================================================
 * 文件策略: 细粒度操作拦截 (读/写/删/改名), (dev,ino) 主键
 *
 * 判定统一走 victim_mask(dentry, fp): 受害者自身 inode 在 file_ino 维
 * 并查 (基线+域), 祖先目录走链在 dir_ino 维并查, 取并集。
 * file_open 额外查字符串 file_str 维 (加载时 stat 失败的兜底规则),
 * 并用 bpf_d_path 拿全路径做事件 detail (sleepable, 允许)。
 * =================================================================== */

/* 文件钩子统一开场: 全局开关 + 进程树 scope + 文件两维 inner 定位 */
#define FILE_HOOK_PREAMBLE()                       \
    const struct filter_config *cfg = get_cfg();   \
    if (!cfg || !cfg->enable_file)                 \
        return 0;                                  \
    struct file_policy fp = {};                    \
    if (!file_policy_setup(cfg, &fp))              \
        return 0

/* lsm.s/file_open: 按 f_flags 区分读/写意图。
 * 注意: open(O_TRUNC) / ">" 重定向 / cp 覆盖 / 编辑器写 都走这里。 */
SEC("lsm.s/file_open")
int BPF_PROG(enforce_file_open, struct file *file)
{
    FILE_HOOK_PREAMBLE();

    /* O_PATH 打开不携带读写意图 (仅用于 stat/dirfd), 不参与判定 */
    __u32 flags = BPF_CORE_READ(file, f_flags);
    if (flags & O_PATH)
        return 0;

    char path[256];
    long ret = bpf_d_path(&file->f_path, path, sizeof(path));
    if (ret < 0) {
        /* bpf_d_path 失败不能静默放行 (fail-open 洞): inode 判定不依赖
         * 路径字符串, 照常执行; detail 用占位符, 字符串兜底只是用不了 */
        __builtin_memcpy(path, "<d_path_err>", 13);
    }

    /* 字符串兜底 (file_str 维) 的基线/域 inner */
    __u32 base = physical_slot(cfg, BASE_SLOT);
    struct bpf_map *base_fstr = bpf_map_lookup_elem(&dom_file_str, &base);
    struct bpf_map *dom_fstr  = bpf_map_lookup_elem(&dom_file_str, &fp.slot);

    /* (dev,ino) 主键 (含目录祖先) | 字符串兜底 */
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(BPF_CORE_READ(file, f_path.dentry), &fp, &rule_ver);
    if (ret >= 0)
        mask |= file_deny_mask(path, base_fstr, dom_fstr, &rule_ver);
    if (!mask) {
        report(1, 0, 1, path, fp.policy_ver, 0);  /* OK */
        return 0;
    }

    __u32 acc = flags & O_ACCMODE;
    bool want_write = (acc != O_RDONLY) ||
                      (flags & (O_CREAT | O_TRUNC | O_APPEND));
    if (want_write && (mask & DENY_WRITE)) {
        report(1, FILE_OP_OPEN_WRITE, 0, path, fp.policy_ver, rule_ver);  /* DENY */
        return -1;
    }
    if (!want_write && (mask & DENY_READ)) {
        report(1, FILE_OP_OPEN_READ, 0, path, fp.policy_ver, rule_ver);   /* DENY */
        return -1;
    }

    report(1, 0, 1, path, fp.policy_ver, rule_ver);  /* OK: 命中名单但该操作不在 deny 列表 */
    return 0;
}

/* lsm/file_permission: 对已经打开的 fd 在每次实际读写前重新授权。
 * 这关闭了“先 open，再进入受控域，随后通过旧 fd 继续 read/write”的绕过。
 *
 * 该 hook 位于高频 VFS 热路径：非读写请求最先返回；允许操作不产生事件。
 * file_open 等判决点已覆盖普通 ALLOW 审计，避免每次 read/write 都消耗采样
 * 预算并制造无意义事件。 */
SEC("lsm/file_permission")
int BPF_PROG(enforce_file_permission, struct file *file, int mask)
{
    if (!(mask & (VFS_MAY_READ | VFS_MAY_WRITE)))
        return 0;

    FILE_HOOK_PREAMBLE();

    if (!file)
        return 0;
    struct dentry *dentry = BPF_CORE_READ(file, f_path.dentry);
    if (!dentry)
        return 0;

    __u32 rule_ver = 0;
    __u8 deny = victim_mask(dentry, &fp, &rule_ver);
    if ((mask & VFS_MAY_WRITE) && (deny & DENY_WRITE)) {
        report_path_op(FILE_OP_FD_WRITE, 0, dentry, fp.policy_ver, rule_ver);
        return -1;
    }
    if ((mask & VFS_MAY_READ) && (deny & DENY_READ)) {
        report_path_op(FILE_OP_FD_READ, 0, dentry, fp.policy_ver, rule_ver);
        return -1;
    }
    return 0;
}

/* lsm/path_truncate: truncate()/ftruncate() 不开文件直接截断,
 * file_open 抓不到, 单独拦。非 sleepable (该钩子不在内核 sleepable
 * 名单里), bpf_d_path 被禁, 判定全靠 (dev,ino)。 */
SEC("lsm/path_truncate")
int BPF_PROG(enforce_path_truncate, const struct path *path)
{
    FILE_HOOK_PREAMBLE();

    struct dentry *dentry = BPF_CORE_READ(path, dentry);
    if (!dentry)
        return 0;

    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    /* DEBUG: 排障用, 正常时注释掉。bpf_trace_printk 不支持 %#x, 用 %x */
    // bpf_printk("[trunc] mask=%x", mask);
    if (mask & DENY_WRITE) {
        report_path_op(FILE_OP_TRUNCATE, 0, dentry, fp.policy_ver, rule_ver);  /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_TRUNCATE, 1, dentry, fp.policy_ver, rule_ver);      /* OK */
    return 0;
}

/* lsm/path_unlink: rm 删文件。不触发 file_open —— 这是旧实现的大缺口。
 * 非 sleepable (同 path_truncate) */
SEC("lsm/path_unlink")
int BPF_PROG(enforce_path_unlink, const struct path *dir,
             struct dentry *dentry)
{
    FILE_HOOK_PREAMBLE();

    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    // bpf_printk("[unlink] mask=%x", mask);
    if (mask & (DENY_DELETE | DENY_RENAME)) {
        report_path_op(FILE_OP_UNLINK, 0, dentry, fp.policy_ver, rule_ver);  /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_UNLINK, 1, dentry, fp.policy_ver, rule_ver);      /* OK */
    return 0;
}

/* lsm/path_rmdir: rmdir 删目录。非 sleepable (同 path_truncate) */
SEC("lsm/path_rmdir")
int BPF_PROG(enforce_path_rmdir, const struct path *dir,
             struct dentry *dentry)
{
    FILE_HOOK_PREAMBLE();

    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    // bpf_printk("[rmdir] mask=%x", mask);
    if (mask & (DENY_DELETE | DENY_RENAME)) {
        report_path_op(FILE_OP_RMDIR, 0, dentry, fp.policy_ver, rule_ver);   /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_RMDIR, 1, dentry, fp.policy_ver, rule_ver);       /* OK */
    return 0;
}

/* lsm/path_rename: mv 改名/移动, 双向判定 ——
 * 源侧视为"删除" (DENY_DELETE|DENY_RENAME),
 * 目标侧视为"写入" (DENY_WRITE|DENY_RENAME)。
 * vim 保存 = 写临时文件再 rename 覆盖, 必须靠目标侧拦截。
 * 目标不存在时 new_dentry->d_inode 为 NULL: 自身查询跳过,
 * 祖先目录判定照常 (victim_mask 内部处理)。
 * 非 sleepable (同 path_truncate) */
SEC("lsm/path_rename")
int BPF_PROG(enforce_path_rename, const struct path *old_dir,
             struct dentry *old_dentry, const struct path *new_dir,
             struct dentry *new_dentry, unsigned int flags)
{
    FILE_HOOK_PREAMBLE();

    __u32 rule_ver = 0;
    __u8 mask = victim_mask(old_dentry, &fp, &rule_ver);
    // bpf_printk("[rename] src mask=%x", mask);
    if (mask & (DENY_DELETE | DENY_RENAME)) {
        report_path_op(FILE_OP_RENAME, 0, old_dentry, fp.policy_ver, rule_ver);  /* DENY (源侧) */
        return -1;
    }

    rule_ver = 0;
    mask = victim_mask(new_dentry, &fp, &rule_ver);
    // bpf_printk("[rename] dst mask=%x", mask);
    if (mask & (DENY_WRITE | DENY_RENAME)) {
        report_path_op(FILE_OP_RENAME, 0, new_dentry, fp.policy_ver, rule_ver);  /* DENY (目标侧) */
        return -1;
    }
    report_path_op(FILE_OP_RENAME, 1, new_dentry, fp.policy_ver, rule_ver);      /* OK */
    return 0;
}

/* ===================================================================
 * L1 议题一补齐钩子: ftruncate / link / symlink / mkdir /
 * chmod / chown / setxattr / mmap 写。
 * 判定骨架与上面五个钩子一致: victim_mask(dentry) 取掩码,
 * 按操作位判定。全部非 sleepable, 只靠 (dev,ino), 不用 bpf_d_path。
 * =================================================================== */

/* lsm/file_truncate: ftruncate(fd) —— fd 在策略装载前已打开时,
 * 不经过 file_open / path_truncate, 单独堵。(内核 >= 6.2 有此钩子) */
SEC("lsm/file_truncate")
int BPF_PROG(enforce_file_truncate, struct file *file)
{
    FILE_HOOK_PREAMBLE();

    struct dentry *dentry = BPF_CORE_READ(file, f_path.dentry);
    if (!dentry)
        return 0;
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    if (mask & DENY_WRITE) {
        report_path_op(FILE_OP_FTRUNCATE, 0, dentry, fp.policy_ver, rule_ver);   /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_FTRUNCATE, 1, dentry, fp.policy_ver, rule_ver);       /* OK */
    return 0;
}

/* lsm/path_link: ln 建硬链。
 * 源侧查 DENY_RENAME (给受保护 inode 起新别名 = 别名操纵;
 * 注意 (dev,ino) 主键下硬链带不走内容防护, 此判定主要为语义完整)。
 * 目标侧父目录查 DENY_WRITE (在受保护目录内建新条目)。 */
SEC("lsm/path_link")
int BPF_PROG(enforce_path_link, struct dentry *old_dentry,
             const struct path *new_dir, struct dentry *new_dentry)
{
    FILE_HOOK_PREAMBLE();

    __u32 rule_ver = 0;
    __u8 mask = victim_mask(old_dentry, &fp, &rule_ver);
    if (mask & DENY_RENAME) {
        report_path_op(FILE_OP_LINK, 0, old_dentry, fp.policy_ver, rule_ver);    /* DENY (源侧) */
        return -1;
    }
    struct dentry *ndir = BPF_CORE_READ(new_dir, dentry);
    rule_ver = 0;
    mask = victim_mask(ndir, &fp, &rule_ver);
    if (mask & DENY_WRITE) {
        report_path_op(FILE_OP_LINK, 0, ndir, fp.policy_ver, rule_ver);          /* DENY (目标父目录) */
        return -1;
    }
    report_path_op(FILE_OP_LINK, 1, old_dentry, fp.policy_ver, rule_ver);        /* OK */
    return 0;
}

/* lsm/path_symlink: ln -s 建软链。新条目不经过 file_open,
 * 受害者在创建时刻还不存在 inode, 只能查父目录: DENY_WRITE。 */
SEC("lsm/path_symlink")
int BPF_PROG(enforce_path_symlink, const struct path *dir,
             struct dentry *dentry, const char *old_name)
{
    FILE_HOOK_PREAMBLE();

    struct dentry *pd = BPF_CORE_READ(dir, dentry);
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(pd, &fp, &rule_ver);
    if (mask & DENY_WRITE) {
        report_path_op(FILE_OP_SYMLINK, 0, pd, fp.policy_ver, rule_ver);         /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_SYMLINK, 1, pd, fp.policy_ver, rule_ver);             /* OK */
    return 0;
}

/* lsm/path_mkdir: mkdir 同理, 查父目录 DENY_WRITE。 */
SEC("lsm/path_mkdir")
int BPF_PROG(enforce_path_mkdir, const struct path *dir,
             struct dentry *dentry, umode_t mode)
{
    FILE_HOOK_PREAMBLE();

    struct dentry *pd = BPF_CORE_READ(dir, dentry);
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(pd, &fp, &rule_ver);
    if (mask & DENY_WRITE) {
        report_path_op(FILE_OP_MKDIR, 0, pd, fp.policy_ver, rule_ver);           /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_MKDIR, 1, pd, fp.policy_ver, rule_ver);               /* OK */
    return 0;
}

/* lsm/path_chmod: chmod 改权限位。查 DENY_ATTR。 */
SEC("lsm/path_chmod")
int BPF_PROG(enforce_path_chmod, const struct path *path, umode_t mode)
{
    FILE_HOOK_PREAMBLE();

    struct dentry *dentry = BPF_CORE_READ(path, dentry);
    if (!dentry)
        return 0;
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    if (mask & DENY_ATTR) {
        report_path_op(FILE_OP_CHMOD, 0, dentry, fp.policy_ver, rule_ver);       /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_CHMOD, 1, dentry, fp.policy_ver, rule_ver);           /* OK */
    return 0;
}

/* lsm/path_chown: chown 改属主。查 DENY_ATTR。
 * uid/gid 形参是 by-value 结构体 (kuid_t/kgid_t), BPF_PROG 的 void*
 * 转换吃不下, 这里只作占位不读取, 故声明为 unsigned long。 */
SEC("lsm/path_chown")
int BPF_PROG(enforce_path_chown, const struct path *path,
             unsigned long uid, unsigned long gid)
{
    FILE_HOOK_PREAMBLE();

    struct dentry *dentry = BPF_CORE_READ(path, dentry);
    if (!dentry)
        return 0;
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    if (mask & DENY_ATTR) {
        report_path_op(FILE_OP_CHOWN, 0, dentry, fp.policy_ver, rule_ver);       /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_CHOWN, 1, dentry, fp.policy_ver, rule_ver);           /* OK */
    return 0;
}

/* lsm/inode_setxattr: setxattr 改扩展属性。查 DENY_ATTR。
 * 签名注意: 内核 6.x (idmapped mounts 改造后) 首参是 mnt_idmap*,
 * 旧 5 参签名 (dentry 打头) BTF 对不上, attach 直接失败。 */
SEC("lsm/inode_setxattr")
int BPF_PROG(enforce_inode_setxattr, struct mnt_idmap *idmap,
             struct dentry *dentry, const char *name,
             const void *value, unsigned long size, int flags)
{
    FILE_HOOK_PREAMBLE();

    if (!dentry)
        return 0;
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    if (mask & DENY_ATTR) {
        report_path_op(FILE_OP_SETXATTR, 0, dentry, fp.policy_ver, rule_ver);    /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_SETXATTR, 1, dentry, fp.policy_ver, rule_ver);        /* OK */
    return 0;
}

/* lsm/inode_removexattr: setfattr -x 删扩展属性。查 DENY_ATTR。 */
SEC("lsm/inode_removexattr")
int BPF_PROG(enforce_inode_removexattr, struct mnt_idmap *idmap,
             struct dentry *dentry, const char *name)
{
    FILE_HOOK_PREAMBLE();

    if (!dentry)
        return 0;
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    if (mask & DENY_ATTR) {
        report_path_op(FILE_OP_REMOVEXATTR, 0, dentry, fp.policy_ver, rule_ver); /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_REMOVEXATTR, 1, dentry, fp.policy_ver, rule_ver);     /* OK */
    return 0;
}

/* lsm/inode_set_acl: setfacl 改 ACL (不走 path_chmod)。查 DENY_ATTR。 */
SEC("lsm/inode_set_acl")
int BPF_PROG(enforce_inode_set_acl, struct mnt_idmap *idmap,
             struct dentry *dentry, const char *acl_name,
             struct posix_acl *kacl)
{
    FILE_HOOK_PREAMBLE();

    if (!dentry)
        return 0;
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    if (mask & DENY_ATTR) {
        report_path_op(FILE_OP_SETACL, 0, dentry, fp.policy_ver, rule_ver);      /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_SETACL, 1, dentry, fp.policy_ver, rule_ver);          /* OK */
    return 0;
}

/* lsm/path_mknod: mknod(2) + open(O_CREAT) 的创建入口
 * (vfs_creat 内部调 security_path_mknod)。与 file_open 的创建判定
 * 互为纵深: 新条目尚无 inode, 查父目录 DENY_WRITE。 */
SEC("lsm/path_mknod")
int BPF_PROG(enforce_path_mknod, const struct path *dir,
             struct dentry *dentry, umode_t mode, unsigned int dev)
{
    FILE_HOOK_PREAMBLE();

    struct dentry *pd = BPF_CORE_READ(dir, dentry);
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(pd, &fp, &rule_ver);
    if (mask & DENY_WRITE) {
        report_path_op(FILE_OP_MKNOD, 0, pd, fp.policy_ver, rule_ver);           /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_MKNOD, 1, pd, fp.policy_ver, rule_ver);               /* OK */
    return 0;
}

/* lsm/mmap_file: mmap 内存直写 —— 绕过 write() 的唯一通道。
 * 只拦 PROT_WRITE + MAP_SHARED (私有映射写不穿到文件, 放行);
 * 匿名映射 file=NULL 直接放行。
 * 注意: 每个可执行文件/动态库加载都走这里 (只读映射), 判定必须
 * 先卡 prot/flags 再查表, 把误拦与开销都压到最小。 */
SEC("lsm/mmap_file")
int BPF_PROG(enforce_mmap_file, struct file *file, unsigned long reqprot,
             unsigned long prot, unsigned long flags)
{
    FILE_HOOK_PREAMBLE();

    if (!((prot & PROT_WRITE) && (flags & MAP_SHARED)))
        return 0;
    if (!file)
        return 0;
    struct dentry *dentry = BPF_CORE_READ(file, f_path.dentry);
    if (!dentry)
        return 0;
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    if (mask & DENY_WRITE) {
        report_path_op(FILE_OP_MMAP_WRITE, 0, dentry, fp.policy_ver, rule_ver);  /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_MMAP_WRITE, 1, dentry, fp.policy_ver, rule_ver);      /* OK */
    return 0;
}

/* lsm/file_mprotect: mprotect/pkey_mprotect 把已有映射升级为可写时触发,
 * 堵 "先 mmap(PROT_READ) 再 mprotect(+PROT_WRITE)" 的逃逸 —— mmap_file
 * 只查建立映射一刻, 升级路径原来无任何钩子。非 sleepable, 判定纯 inode。
 * 与 mmap_file 职责互补: 建立即 PROT_WRITE 的归 mmap_file, 事后提权的归这里。 */
SEC("lsm/file_mprotect")
int BPF_PROG(enforce_file_mprotect, struct vm_area_struct *vma,
             unsigned long reqprot, unsigned long prot)
{
    FILE_HOOK_PREAMBLE();

    if (!(prot & PROT_WRITE))        /* 只关心写提权 */
        return 0;
    if (!vma)
        return 0;
    unsigned long vm_flags = BPF_CORE_READ(vma, vm_flags);
    if (!(vm_flags & VM_SHARED))     /* MAP_PRIVATE 提权是 COW, 不碰文件 */
        return 0;
    struct file *file = BPF_CORE_READ(vma, vm_file);
    if (!file)                       /* 匿名映射 */
        return 0;
    struct dentry *dentry = BPF_CORE_READ(file, f_path.dentry);
    if (!dentry)
        return 0;
    __u32 rule_ver = 0;
    __u8 mask = victim_mask(dentry, &fp, &rule_ver);
    if (mask & DENY_WRITE) {
        report_path_op(FILE_OP_MPROTECT, 0, dentry, fp.policy_ver, rule_ver);  /* DENY */
        return -1;
    }
    report_path_op(FILE_OP_MPROTECT, 1, dentry, fp.policy_ver, rule_ver);      /* OK */
    return 0;
}

/* ===================================================================
 * 命令策略: lsm/bprm_check_security
 *
 * dom_cmd_ino 按真实可执行 inode 主判，dom_cmd 为 stat 失败字符串兜底；
 * dom_arg 用 inode/string 双身份做参数级前缀匹配。
 * 每维基线+域 inner 各查一次传入复用 (尤其参数级的 4 级回退查询)。
 * =================================================================== */

/* exec 事件的统一上报 (kind=2): detail=解析后命令路径, args=暂存的
 * 参数 token。ea 非空时展示用 tok[0] 也统一覆盖成解析后路径。
 * domain_id 由调用方从 in_scope_exec 的 scope_info 传入: exec 判定
 * 有 pending 兜底 (fork→exec 窗口内 tracked_pids 还没落表), 上报时刻
 * 再查 tracked 会错归属, 必须直接用判定时刻的域身份 (Web 控制台设计 §5)。 */
static __always_inline void report_exec(__u32 allowed, const char *fn,
                                        struct exec_args *ea,
                                        __u32 policy_ver, __u32 rule_ver,
                                        __u64 domain_id)
{
    bool rule_audit = (rule_ver & 0x80000000u) != 0;
    rule_ver &= 0x7fffffffu;
    if (allowed && !rule_audit && !should_report_allow())
        return;

    struct event *e = bpf_ringbuf_reserve(&events, sizeof(*e), 0);
    if (!e) {
        count_event_drop();
        return;
    }
    __u64 id = bpf_get_current_pid_tgid();
    e->tgid = id >> 32;
    e->pid  = (__u32)id;
    e->ts_ns = bpf_ktime_get_ns();
    e->cgroup_id = bpf_get_current_cgroup_id();
    e->kind = 2;
    e->op = 0;
    e->allowed = allowed;
    e->policy_version = policy_ver;
    e->rule_version = rule_ver;
    e->domain_id = domain_id;
    bpf_get_current_comm(e->comm, sizeof(e->comm));
    bpf_probe_read_kernel_str(e->detail, sizeof(e->detail), fn);
    if (ea) {
        /* 展示也统一用解析后路径 (ea 指向 map value, 可写) */
        bpf_probe_read_kernel_str(ea->tok[0], ARG_TOKEN_LEN, fn);
        __builtin_memcpy(e->args, ea->tok, sizeof(e->args));
    } else {
        __builtin_memset(e->args, 0, sizeof(e->args));
    }
    bpf_ringbuf_submit(e, 0);
}

SEC("lsm/bprm_check_security")
int BPF_PROG(enforce_bprm_check, struct linux_binprm *bprm)
{
    const struct filter_config *cfg = get_cfg();
    if (!cfg || !cfg->enable_exec)
        return 0;

    struct scope_info si = {};
    if (!in_scope_exec(&si))
        return 0;

    /* 命令/参数两维 outer: 基线 slot + 域 slot 各查一次 */
    __u32 base = physical_slot(cfg, BASE_SLOT);
    __u32 domain = physical_slot(cfg, si.policy_slot);
    struct bpf_map *base_cmd = bpf_map_lookup_elem(&dom_cmd, &base);
    struct bpf_map *dom_cmd_m = bpf_map_lookup_elem(&dom_cmd, &domain);
    struct bpf_map *base_cmd_ino = bpf_map_lookup_elem(&dom_cmd_ino, &base);
    struct bpf_map *dom_cmd_ino_m = bpf_map_lookup_elem(&dom_cmd_ino, &domain);
    struct bpf_map *base_arg = bpf_map_lookup_elem(&dom_arg, &base);
    struct bpf_map *dom_arg_m = bpf_map_lookup_elem(&dom_arg, &domain);
    struct slot_meta *sm = bpf_map_lookup_elem(&slot_meta_map, &domain);
    __u32 policy_ver = sm ? sm->version : 0;
    __u32 rule_ver = 0;

    struct file *executable = BPF_CORE_READ(bprm, file);
    const char *filename = BPF_CORE_READ(bprm, filename);
    char fn[64] = {};
    long fn_ret = filename ?
        bpf_probe_read_kernel_str(fn, sizeof(fn), filename) : -1;
    if (fn_ret <= 0)
        __builtin_memcpy(fn, "<exec-path-unavailable>", 24);

    __u64 tid = bpf_get_current_pid_tgid();
    struct exec_args *ea = bpf_map_lookup_elem(&exec_args_map, &tid);

    bool command_denied = is_cmd_inode_in_blacklist(
        executable, base_cmd_ino, dom_cmd_ino_m, &rule_ver);
    if (!command_denied && fn_ret > 0)
        command_denied = is_cmd_in_blacklist(fn, base_cmd, dom_cmd_m, &rule_ver);
    if (command_denied) {
        report_exec(0, fn, ea, policy_ver, rule_ver, si.scope_id);   /* DENY (整命令) */
        if (ea)
            bpf_map_delete_elem(&exec_args_map, &tid);
        return -1;
    }

    /* 参数级黑名单: 从 capture_exec_args 暂存的 argv 匹配, 取出后即删。
     * 注意 bprm->p 路线已证伪 (见 docs/archive/命令参数拦截调研.md §2.1):
     * bprm->p 是新 mm 的用户地址, 本 hook 触发时新 mm 未安装, 读不到,
     * 所以参数只能从 sys_enter_execve 提前暂存。
     * tok[0] 不用 argv[0] (可伪造/随敲法变化), 统一用 bprm->filename。 */
    {
        if (ea && is_args_in_blacklist(ea, executable,
                                       fn_ret > 0 ? fn : NULL,
                                       base_arg, dom_arg_m, &rule_ver)) {
            report_exec(0, fn, ea, policy_ver, rule_ver, si.scope_id);  /* DENY (参数级) */
            bpf_map_delete_elem(&exec_args_map, &tid);
            return -1;
        }

        /* EXEC 审计: 未命中任何黑名单也上报 allowed=1 事件。
         * 否则 bprm 钩子"放行即静默", ls/cat 这类正常命令只会以
         * file_open(exec 时要打开二进制) 的形式出现在 FILE 事件里。
         * 在判决点上报, allowed 状态必然准确; 参数从暂存里带上展示。 */
        report_exec(1, fn, ea, policy_ver, rule_ver, si.scope_id);      /* OK */
        if (ea)
            bpf_map_delete_elem(&exec_args_map, &tid);
    }

    return 0;
}

/* ===================================================================
 * 取参: tracepoint/syscalls/sys_enter_execve + sys_enter_execveat
 *
 * 为什么需要它: lsm/bprm_check_security 里拿不到 argv (bprm->p 是
 * 新 mm 的用户地址, hook 触发时新 mm 尚未安装, 无可读途径)。
 * syscall 入口时 argv 指针数组还在当前(旧) mm 的用户地址空间里,
 * bpf_probe_read_user 可读。argv 的 syscall 参数位置分别为:
 *   execve(const char *filename, const char *const argv[],
 *          const char *const envp[])
 *   execve:   args[0]=filename  args[1]=argv
 *   execveat: args[0]=dirfd     args[1]=filename  args[2]=argv
 *
 * 只采集 argv[1..3] (真实参数) 到 tok[1..3], tok[0] 留空 ——
 * argv[0] 可被 exec -a 伪造、也随敲法 (git vs /usr/bin/git) 变化,
 * tok[0] 由 LSM 判决点用可信的 bprm->filename 填充。
 * 两者是同一任务同一次系统调用的前后两点, 顺序有保证。
 * =================================================================== */
static __always_inline int capture_exec_argv(const char *const *argv)
{
    const struct filter_config *cfg = get_cfg();
    if (!cfg)
        return 0;
    if (!cfg->enable_exec)
        return 0;
    /* exec 路径 scope: tracked 优先, pending 兜底 —— 新 fork 的子孙
     * 第一次 exec 时 sched_process_exec 尚未落表 (见 in_scope_exec 注释),
     * 纯 tracked 判定会让 argv 暂存错过整个首次 exec */
    struct scope_info si = {};
    if (!in_scope_exec(&si))
        return 0;

    if (!argv)
        return 0;

    struct exec_args ea = {};
    bool stop = false;
#pragma unroll
    for (int i = 1; i < ARG_TOKEN_N; i++) {
        if (!stop) {
            const char *p = NULL;
            bpf_probe_read_user(&p, sizeof(p), &argv[i]);
            if (!p) {
                stop = true;
            } else {
                /* 定长槽, verifier 友好; 拼接展示在 Rust audit 做 */
                bpf_probe_read_user_str(ea.tok[i], ARG_TOKEN_LEN, p);
            }
        }
    }

    /* DEBUG: 排障时取消注释 */
    // bpf_printk("[execve] tok1=%s tok2=%s", ea.tok[1], ea.tok[2]);

    __u64 id = bpf_get_current_pid_tgid();
    bpf_map_update_elem(&exec_args_map, &id, &ea, BPF_ANY);
    return 0;
}

SEC("tracepoint/syscalls/sys_enter_execve")
int capture_exec_args(struct trace_event_raw_sys_enter *ctx)
{
    return capture_exec_argv((const char *const *)ctx->args[1]);
}

SEC("tracepoint/syscalls/sys_enter_execveat")
int capture_execveat_args(struct trace_event_raw_sys_enter *ctx)
{
    return capture_exec_argv((const char *const *)ctx->args[2]);
}

/* ===================================================================
 * 网络策略: lsm/socket_connect
 *
 * allow/deny 名单模式, 两级判定 (P2 map-in-map):
 *   相位 1 端口规则 (dom_net_port): 域 inner → 基线 inner
 *   相位 2 无端口规则 (dom_net):    域 inner → 基线 inner
 * 每相位内 "域优先于基线" (域是更具体的作用域, 可覆盖基线结论);
 * 相位顺序保持旧版 "带端口规则优先于无端口规则 (先精确后通配)"。
 * 命中条目的 action 决定放行/拒绝; 都未命中 = 默认允许。
 * 白名单模式 = 基线配一条 "deny 0.0.0.0/0" (prefixlen 0 匹配一切)
 * 兜底, 域里逐条 allow 放行。
 * IPv4/IPv6 分别使用独立 LPM 维度，优先级与默认动作完全一致。
 * loopback 也走规则检查 (白名单模式下需自行 allow)。
 * =================================================================== */
static __always_inline int enforce_ipv6_connect(const struct filter_config *cfg,
                                                const struct scope_info *si,
                                                struct sockaddr *addr,
                                                int addrlen)
{
    /* sockaddr_in6: family(2), port(2), flowinfo(4), addr(16), scope(4)。
     * 只读取到 address 末尾，因此 24 字节是本程序需要的最小可信长度。 */
    if (addrlen < 24)
        return 0;

    __u32 base = physical_slot(cfg, BASE_SLOT);
    __u32 domain = physical_slot(cfg, si->policy_slot);
    struct bpf_map *base_port = bpf_map_lookup_elem(&dom_net6_port, &base);
    struct bpf_map *dom_port = bpf_map_lookup_elem(&dom_net6_port, &domain);
    struct bpf_map *base_addr = bpf_map_lookup_elem(&dom_net6, &base);
    struct bpf_map *dom_addr = bpf_map_lookup_elem(&dom_net6, &domain);
    struct slot_meta *sm = bpf_map_lookup_elem(&slot_meta_map, &domain);
    __u32 policy_ver = sm ? sm->version : 0;
    __u32 rule_ver = 0;

    __u8 daddr[16] = {};
    long ret = bpf_probe_read_kernel(daddr, sizeof(daddr),
                                     (const void *)addr + 8);
    if (ret < 0)
        return 0;
    __u16 dport_be = 0;
    bpf_probe_read_kernel(&dport_be, sizeof(dport_be),
                          (const void *)addr + 2);
    __u16 dport = __bpf_ntohs(dport_be);

    char detail[40] = {};
    fmt_ipv6_hex(detail, daddr);
    detail[32] = ':';
    fmt_port5(detail + 33, dport);
    detail[38] = '\0';

    struct net6_port_lpm_key pkey = { .prefixlen = 144, .port = dport };
    __builtin_memcpy(pkey.addr, daddr, sizeof(pkey.addr));
    struct net_rule_val *v = 0;
    struct net_rule_val *base_v = base_port ? bpf_map_lookup_elem(base_port, &pkey) : 0;
    struct net_rule_val *dom_v = dom_port ? bpf_map_lookup_elem(dom_port, &pkey) : 0;
    if (base_v && base_v->action == 0) { rule_ver = base_v->version | (base_v->audit ? 0x80000000u : 0); goto deny6; }
    if (dom_v && dom_v->action == 0) { rule_ver = dom_v->version | (dom_v->audit ? 0x80000000u : 0); goto deny6; }
    v = dom_v ? dom_v : base_v;
    if (v) { rule_ver = v->version | (v->audit ? 0x80000000u : 0); goto allow6; }

    struct net6_lpm_key key = { .prefixlen = 128 };
    __builtin_memcpy(key.addr, daddr, sizeof(key.addr));
    base_v = base_addr ? bpf_map_lookup_elem(base_addr, &key) : 0;
    dom_v = dom_addr ? bpf_map_lookup_elem(dom_addr, &key) : 0;
    if (base_v && base_v->action == 0) { rule_ver = base_v->version | (base_v->audit ? 0x80000000u : 0); goto deny6; }
    if (dom_v && dom_v->action == 0) { rule_ver = dom_v->version | (dom_v->audit ? 0x80000000u : 0); goto deny6; }
    v = dom_v ? dom_v : base_v;
    if (!v)
        goto allow6;
    rule_ver = v->version | (v->audit ? 0x80000000u : 0);
    if (v->action == 0)
        goto deny6;

allow6:
    report(3, 0, 1, detail, policy_ver, rule_ver);
    return 0;

deny6:
    report(3, 0, 0, detail, policy_ver, rule_ver);
    return -1;
}

SEC("lsm/socket_connect")
int BPF_PROG(enforce_socket_connect, struct socket *sock, struct sockaddr *addr, int addrlen)
{
    const struct filter_config *cfg = get_cfg();
    if (!cfg)
        return 0;
    if (!cfg->enable_net)
        return 0;

    struct scope_info si = {};
    if (!current_scope_info(&si))
        return 0;

    struct sock *skp = BPF_CORE_READ(sock, sk);
    if (!skp)
        return 0;
    short family = BPF_CORE_READ(skp, __sk_common.skc_family);

    /* DEBUG: 排障时取消注释。打印每次 connect 的 family / addrlen */
    // bpf_printk("[net] ENTER family=%d addrlen=%d", family, addrlen);

    if (family == 10)
        return enforce_ipv6_connect(cfg, &si, addr, addrlen);
    if (family != 2)
        return 0;

    /* addrlen 防御性检查: LSM socket_connect 在协议层地址长度校验
     * 之前触发, AF_INET 但 addrlen 过短时 sockaddr_storage 里残余
     * 的是栈上旧数据, 读出的 daddr/dport 不可信, 直接放行
     * (这种 connect 随后会被协议层 EINVAL 拒掉, 无放行风险) */
    if (addrlen < 8)
        return 0;

    /* 两维 outer: 基线 slot + 域 slot 各查一次, 拿到 inner 指针 */
    __u32 base = physical_slot(cfg, BASE_SLOT);
    __u32 domain = physical_slot(cfg, si.policy_slot);
    struct bpf_map *base_port = bpf_map_lookup_elem(&dom_net_port, &base);
    struct bpf_map *dom_port  = bpf_map_lookup_elem(&dom_net_port, &domain);
    struct bpf_map *base_addr = bpf_map_lookup_elem(&dom_net, &base);
    struct bpf_map *dom_addr  = bpf_map_lookup_elem(&dom_net, &domain);
    struct slot_meta *sm = bpf_map_lookup_elem(&slot_meta_map, &domain);
    __u32 policy_ver = sm ? sm->version : 0;
    __u32 rule_ver = 0;

    /* 注意: LSM hook 里 addr 是内核地址 (move_addr_to_kernel 已拷贝),
     * 必须用 bpf_probe_read_kernel; probe_read_user 会返回 -EFAULT */
    __u32 daddr = 0;
    long ret = bpf_probe_read_kernel(&daddr, sizeof(daddr),
                                     (const void *)addr + 4);
    if (ret < 0)
        return 0;

    /* sin_port 在偏移 2, 网络字节序; 转成主机序再作 map key
     * (Rust 网络编译器直接存端口号数值) */
    __u16 dport_be = 0;
    bpf_probe_read_kernel(&dport_be, sizeof(dport_be),
                          (const void *)addr + 2);
    __u16 dport = __bpf_ntohs(dport_be);

    /* detail: 格式化成 ASCII "008.008.004.004:00443" (定宽, 全常量
     * 偏移, daddr 按内存字节序, b0 在第一段) */
    char detail[24] = {};
    fmt_octet3(detail,      daddr & 0xff);
    detail[3]  = '.';
    fmt_octet3(detail + 4,  (daddr >> 8)  & 0xff);
    detail[7]  = '.';
    fmt_octet3(detail + 8,  (daddr >> 16) & 0xff);
    detail[11] = '.';
    fmt_octet3(detail + 12, (daddr >> 24) & 0xff);
    detail[15] = ':';
    fmt_port5(detail + 16, dport);
    detail[21] = '\0';

    /* 两级查找: 相位 1 端口规则 (域→基线), 相位 2 无端口规则 (域→基线)。
     *
     * verifier 红线 (沿用旧版的教训): 不能把 lookup/比较结果合成 bool
     * 再 "if (x) return -1" —— clang if-convert 出的算术选择会让出口
     * R0 变成 unknown scalar, LSM 程序要求 R0 ∈ [-4095,0], 直接拒载。
     * goto 形式让 deny/allow 各走常量 return。
     * 另一红线 (P2 实测): 多指针判空不得合并写 (!a && b), 会被
     * if-convert 成指针位运算遭拒; 一律嵌套分支。 */
    struct net_port_lpm_key pkey = { .prefixlen = 48, .port = dport };
    __builtin_memcpy(pkey.addr, &daddr, 4);
    struct net_rule_val *v = 0;
    struct net_rule_val *base_v4 = base_port ? bpf_map_lookup_elem(base_port, &pkey) : 0;
    struct net_rule_val *dom_v4 = dom_port ? bpf_map_lookup_elem(dom_port, &pkey) : 0;
    if (base_v4 && base_v4->action == 0) { rule_ver = base_v4->version | (base_v4->audit ? 0x80000000u : 0); goto deny; }
    if (dom_v4 && dom_v4->action == 0) { rule_ver = dom_v4->version | (dom_v4->audit ? 0x80000000u : 0); goto deny; }
    v = dom_v4 ? dom_v4 : base_v4;
    if (v) { rule_ver = v->version | (v->audit ? 0x80000000u : 0); goto allow; }

    struct net_lpm_key key = { .prefixlen = 48, .addr = daddr, .port = dport };
    base_v4 = base_addr ? bpf_map_lookup_elem(base_addr, &key) : 0;
    dom_v4 = dom_addr ? bpf_map_lookup_elem(dom_addr, &key) : 0;
    if (base_v4 && base_v4->action == 0) { rule_ver = base_v4->version | (base_v4->audit ? 0x80000000u : 0); goto deny; }
    if (dom_v4 && dom_v4->action == 0) { rule_ver = dom_v4->version | (dom_v4->audit ? 0x80000000u : 0); goto deny; }
    v = dom_v4 ? dom_v4 : base_v4;
    if (!v)
        goto allow;              /* 未命中: 默认允许 */
    rule_ver = v->version | (v->audit ? 0x80000000u : 0);
    if (v->action == 0)
        goto deny;

allow:
    report(3, 0, 1, detail, policy_ver, rule_ver);  /* OK */
    return 0;

deny:
    report(3, 0, 0, detail, policy_ver, rule_ver);  /* DENY */
    return -1;
}

/* ===================================================================
 * 守护者自保 (台账 D1/D3, L3 路线图议题二; 对标 ActPlane
 * enforce_task_kill / enforce_ptrace_access_check / enforce_bpf_syscall)
 *
 * 与策略钩子不同: 不走 YAML enable_* 开关, always-on; scope 用
 * guard_scope ([pidtree] 进程树作用域, 空集 = 不生效, 见上文)。
 * 防护方向只有 "被管树 → daemon": 树外同 uid 攻击者按进程树
 * 区分不了, 属部署加固半区 (专用用户运行 daemon), 见 README 已知边界。
 * 守护事件与策略无关, 版本字段恒 0。
 * =================================================================== */

/* task_kill: kill/tgkill/pidfd_send_signal 全部汇聚到这一个钩子。
 * sig==0 是存在性探测 (kill -0), 放行; 被管树内互杀不管
 * (target 不在注册表即放行)。 */
SEC("lsm/task_kill")
int BPF_PROG(enforce_task_kill, struct task_struct *target,
             struct kernel_siginfo *info, int sig, const struct cred *cred)
{
    (void)info;
    (void)cred;
    const struct filter_config *cfg = get_cfg();
    if (!cfg)
        return 0;
    if (!guard_scope(cfg))
        return 0;
    if (sig == 0)
        return 0;
    if (!target)
        return 0;
    __u32 tgid = BPF_CORE_READ(target, tgid);
    if (tgid == 0)
        return 0;
    if (!bpf_map_lookup_elem(&protected_pids, &tgid))
        return 0;

    char detail[24] = {};
    __builtin_memcpy(detail, "kill sig=", 9);
    fmt_dec2(detail + 9, (__u32)sig);
    __builtin_memcpy(detail + 11, " tgid=", 6);
    fmt_dec7(detail + 17, tgid);
    report(4, GUARD_OP_KILL, 0, detail, 0, 0);
    return -1;
}

/* ptrace_access_check: 被管树内一律拒, 不看目标 (L2 议题五钦定的
 * 合并规则) —— process_vm_readv/writev 内核内部走 __ptrace_may_access
 * 同一个钩子, 一并盖住; 对 daemon 的 ptrace 自然也含在其中。
 * 代价: 被管内 strace/gdb 不可用 (已知边界, 见 README)。 */
SEC("lsm/ptrace_access_check")
int BPF_PROG(enforce_ptrace_access_check, struct task_struct *child,
             unsigned int mode)
{
    (void)mode;
    const struct filter_config *cfg = get_cfg();
    if (!cfg)
        return 0;
    if (!guard_scope(cfg))
        return 0;

    __u32 tgid = child ? BPF_CORE_READ(child, tgid) : 0;
    char detail[24] = {};
    __builtin_memcpy(detail, "ptrace tgid=", 12);
    fmt_dec7(detail + 12, tgid);
    report(4, GUARD_OP_PTRACE, 0, detail, 0, 0);
    return -1;
}

/* ptrace_traceme: 反向 ptrace (让别的进程 trace 自己以借权), 被管内拒。 */
SEC("lsm/ptrace_traceme")
int BPF_PROG(enforce_ptrace_traceme, struct task_struct *parent)
{
    (void)parent;
    const struct filter_config *cfg = get_cfg();
    if (!cfg)
        return 0;
    if (!guard_scope(cfg))
        return 0;
    report(4, GUARD_OP_TRACEME, 0, "ptrace_traceme", 0, 0);
    return -1;
}

/* bpf(): 被管树内一刀切全拒 (L3 文档: "agent 无任何合法 bpf 需求")。
 * daemon 自身不在追踪集, 其 map 更新/事件读取不受影响。
 *
 * 签名必须是 3 参 —— 本机 6.6 lsm_hook_defs.h 已核实;
 * ActPlane 的 bool privileged 第 4 参是更新内核的钩子形态,
 * 照抄会 attach 失败。 */
SEC("lsm/bpf")
int BPF_PROG(enforce_bpf_guard, int cmd, union bpf_attr *attr,
             unsigned int size)
{
    (void)attr;
    (void)size;
    const struct filter_config *cfg = get_cfg();
    if (!cfg)
        return 0;
    if (!guard_scope(cfg))
        return 0;

    char detail[16] = {};
    __builtin_memcpy(detail, "bpf cmd=", 8);
    fmt_octet3(detail + 8, (__u32)cmd);
    report(4, GUARD_OP_BPF, 0, detail, 0, 0);
    return -1;
}

/* ===================================================================
 * 进程树追踪 (pidtree P1): fork 主表/降级落表 → exec/reconcile 提升 → exit 清理
 *
 * 照搬 AcTrail live_observation 的追踪集合维护机制 (设计文档
 * docs/design/多Agent域模型-进程树作用域设计.md §4.1 伪码)。
 * always-on (与守护钩同等待遇, 不走 YAML 开关)。
 * 与命令捕获用的 tracepoint/syscalls/sys_enter_execve 是不同的
 * attach 点, 互不冲突, 各自保留。
 * fork 只继承 scope_id；policy slot 每次判定时通过 scope_policies 解析。
 * =================================================================== */

/* 1) fork 立即继承: 在 parent 上下文跑, args[0]=parent task,
 * args[1]=child task。新进程当场进入 tracked_pids，因此 fork 后不 exec
 * 也没有 reconcile 窗口。clone 线程的 child_tgid == parent_tgid，必须跳过，
 * 否则会用线程 start_boottime 覆盖整个进程的 generation。 */
SEC("raw_tracepoint/sched_process_fork")
int handle_sched_process_fork(struct bpf_raw_tracepoint_args *ctx)
{
    struct task_struct *parent = (struct task_struct *)ctx->args[0];
    struct task_struct *child  = (struct task_struct *)ctx->args[1];
    /* CO-RE 读 parent/child tgid 与 child 的 start_boottime */
    __u32 parent_tgid = BPF_CORE_READ(parent, tgid);
    __u32 child_tgid  = BPF_CORE_READ(child, tgid);
    __u64 child_start = BPF_CORE_READ(child, start_boottime);

    struct scope_info parent_scope = {};
    if (!task_scope_info(parent_tgid, parent, &parent_scope))
        return 0;
    if (child_tgid == 0 || child_tgid == parent_tgid)
        return 0;
    __u64 child_start_ticks = child_start / NS_PER_USER_TICK;
    __u64 inherited_scope = parent_scope.scope_id;
    struct track_val tv = {
        .scope_id = inherited_scope,
    };
    long start_rc = bpf_map_update_elem(&pid_start_times, &child_tgid,
                                        &child_start_ticks, BPF_ANY);
    if (start_rc != 0)
        record_pid_tracking_stat(PID_STAT_START_FAILURE);
    long track_rc = start_rc == 0
        ? bpf_map_update_elem(&tracked_pids, &child_tgid, &tv, BPF_ANY)
        : start_rc;
    if (start_rc == 0 && track_rc != 0)
        record_pid_tracking_stat(PID_STAT_TRACKED_FAILURE);
    if (start_rc == 0 && track_rc == 0) {
        bpf_map_delete_elem(&pending_child_proc_ops, &child_tgid);
        return 0;
    }
    bpf_map_delete_elem(&tracked_pids, &child_tgid);
    bpf_map_delete_elem(&pid_start_times, &child_tgid);

    /* 容量/瞬时失败降级：pending 对所有策略 hook 都是完整 scope。 */
    struct pending_proc_op op = {
        .scope_id = inherited_scope,
        .child_start_boottime = child_start_ticks,
    };
    long pending_rc = bpf_map_update_elem(&pending_child_proc_ops, &child_tgid,
                                          &op, BPF_ANY);
    if (pending_rc == 0)
        record_pid_tracking_stat(PID_STAT_PENDING_FALLBACK);
    else
        record_pid_tracking_stat(PID_STAT_PENDING_FAILURE);
    return 0;
}

/* 2) exec 落表: 仅处理 fork 阶段写 tracked 失败后留下的 pending。
 * sched_process_exec 在 exec 成功的进程上下文跑,
 * 当前 tgid 即 fork 时暂存的 child_tgid。 */
SEC("tracepoint/sched/sched_process_exec")
int handle_sched_process_exec(void *ctx)
{
    (void)ctx;
    __u32 tgid = bpf_get_current_pid_tgid() >> 32;
    struct pending_proc_op *op =
        bpf_map_lookup_elem(&pending_child_proc_ops, &tgid);
    if (!op)
        return 0;
    struct track_val tv = {
        .scope_id = op->scope_id,
    };
    __u64 start = op->child_start_boottime;
    if (bpf_map_update_elem(&pid_start_times, &tgid, &start, BPF_ANY) != 0) {
        record_pid_tracking_stat(PID_STAT_START_FAILURE);
        return 0;
    }
    if (bpf_map_update_elem(&tracked_pids, &tgid, &tv, BPF_ANY) != 0) {
        record_pid_tracking_stat(PID_STAT_TRACKED_FAILURE);
        bpf_map_delete_elem(&pid_start_times, &tgid);
        return 0;
    }
    /* pending remains authoritative until both primary entries are visible. */
    bpf_map_delete_elem(&pending_child_proc_ops, &tgid);
    return 0;
}

/* 3) exit 清理: pending 直接删除 (fork 后未 exec 就退出的 child 丢弃,
 * 与 AcTrail flush 进表发事件不同 —— 将死进程无需纳入强制范围, §2.3);
 * leader (pid==tgid) 退出才删 tracked, 非 leader 线程退出不管 (§7-2)。 */
SEC("tracepoint/sched/sched_process_exit")
int handle_sched_process_exit(void *ctx)
{
    (void)ctx;
    __u64 pid_tgid = bpf_get_current_pid_tgid();
    __u32 tgid = pid_tgid >> 32;
    bpf_map_delete_elem(&pending_child_proc_ops, &tgid);
    if ((__u32)pid_tgid != tgid)
        return 0;                       /* 非 leader 线程退出不管 */
    bpf_map_delete_elem(&tracked_pids, &tgid);
    bpf_map_delete_elem(&pid_start_times, &tgid);
    return 0;
}
