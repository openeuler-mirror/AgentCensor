/* SPDX-License-Identifier: GPL-2.0 */
/*
 * path_walk.h —— 内核态手工 d_path 走链 (非 sleepable 可用的路径解析)
 */
#ifndef __PATH_WALK_H__
#define __PATH_WALK_H__

#include "vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>

#define PATH_BUF_LEN   4096
#define PATH_WALK_MAX  128

/* 走链 scratch 缓冲。+256 余量: 帮助 verifier 证明
 * prepend_name 的 buffer_offset 不越界。per-cpu 独占, 无需锁;
 * 各钩子不嵌套调用, 共享一个实例。 */
struct path_buf_value {
    char buf[PATH_BUF_LEN + 256];
};
struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, struct path_buf_value);
} path_buf_heap SEC(".maps");

struct path_walk_state {
    char *buf;               /* 缓冲基址 (map value 指针) */
    int blen;                /* 剩余空间, 恒等于写入位置偏移 (bptr - buf) */
    struct dentry *dentry;
    struct vfsmount *mnt;    /* NULL = 无挂载信息, 不可跨挂载 */
    struct mount *mp;        /* real_mount(mnt), mnt 非空时有效 */
    bool resolved;
    bool partial;
};

static __always_inline bool walk_is_root(struct dentry *dentry)
{
    struct dentry *parent = NULL;
    bpf_probe_read_kernel(&parent, sizeof(parent), &dentry->d_parent);
    return dentry == parent;
}

/* 单步 prepend "/name"。verifier 技巧:
 * namelen 用 asm 截断到 0xff (NAME_MAX 之内), buffer_offset 全部显式
 * 有界检查。blen 恒等于写入位置相对 buf 的偏移 (省掉冗余的 bptr
 * 指针, 走链子程序栈帧 -8B)。返回 0 = 成功; -1 = 名字装不下
 * (调用方按 unresolved 处理)。 */
static __always_inline int walk_prepend_name(struct path_walk_state *st,
                                             const char *name, __u32 namelen)
{
    bool write_slash = true;
    __u64 buffer_offset = (__u64)st->blen;   /* == bptr - buf */

    /* 名字装不下时截断保留尾部 (按 unresolved 终止) */
    if ((__s64)namelen >= st->blen) {
        name += namelen - st->blen;
        namelen = st->blen;
        write_slash = false;
    }

    st->blen -= (namelen + write_slash);

    if (namelen + write_slash > buffer_offset)
        return -1;
    buffer_offset -= (namelen + write_slash);
    if (buffer_offset >= PATH_BUF_LEN)
        return -1;

    if (write_slash)
        st->buf[buffer_offset] = '/';

    asm volatile("%[namelen] &= 0xff;\n" : [namelen] "+r"(namelen));
    bpf_probe_read_kernel(st->buf + buffer_offset + write_slash,
                          namelen, name);

    return write_slash ? 0 : -1;
}

/* bpf_loop 回调: 每次调用走一级。返回 1 停止, 0 继续。 */
static long walk_step(__u32 index, void *ctx)
{
    struct path_walk_state *st = ctx;
    struct dentry *dentry = st->dentry;
    struct dentry *parent = NULL;
    struct dentry *mnt_root = NULL;
    struct qstr dname = {};

    if (st->mnt) {
        bpf_probe_read_kernel(&mnt_root, sizeof(mnt_root), &st->mnt->mnt_root);
        if (dentry == mnt_root || walk_is_root(dentry)) {
            /* 到达某挂载的根: 跨到父挂载的挂载点继续向上 */
            struct mount *parent_mp = NULL;
            bpf_probe_read_kernel(&parent_mp, sizeof(parent_mp),
                                  &st->mp->mnt_parent);
            if (st->mp != parent_mp) {
                bpf_probe_read_kernel(&st->dentry, sizeof(st->dentry),
                                      &st->mp->mnt_mountpoint);
                st->mp = parent_mp;
                st->mnt = &parent_mp->mnt;
                return 0;
            }
            /* mnt == mnt_parent: 命名空间根, 解析完成 */
            st->resolved = true;
            return 1;
        }
    } else if (walk_is_root(dentry)) {
        /* 无 mnt 信息, 不能跨挂载: 残缺路径不可信, 不用于匹配 */
        st->partial = true;
        return 1;
    }

    bpf_probe_read_kernel(&parent, sizeof(parent), &dentry->d_parent);
    bpf_probe_read_kernel(&dname, sizeof(dname), &dentry->d_name);
    if (walk_prepend_name(st, (const char *)dname.name, dname.len))
        return 1;   /* 名字装不下: resolved 保持 false, 按 unresolved 终止 */

    st->dentry = parent;
    return 0;
}

/* 解析 dentry+mnt 的命名空间绝对路径。
 *
 * 成功: 返回指向 path_buf_heap 内路径串的指针, *out_len 为长度 (>0),
 *       *out_partial 为 false; 路径在堆内已 NUL 终止
 *       (buf[PATH_BUF_LEN], 由 blen 不变量可知恰为路径末尾后一字节,
 *       +256 margin 保证可写)。
 * 失败: 返回 NULL; *out_partial 区分原因 —— true = mnt 缺失提前终止
 *       (调用方不应表态), false = 深度/长度预算耗尽 (调用方 fail-closed)。
 *
 * 注意: 路径 "/" (起始即命名空间根) 解析为 len=0, 视为不可匹配, 返回 NULL
 * 且 partial=true (对 "/" 的整根规则无实际意义)。 */
static __always_inline char *path_resolve(struct dentry *dentry,
                                          struct vfsmount *mnt,
                                          __u32 *out_len,
                                          bool *out_partial)
{
    __u32 zero = 0;
    *out_partial = false;   /* 默认失败方向: 不可证明安全 (fail-closed) */
    struct path_buf_value *heap = bpf_map_lookup_elem(&path_buf_heap, &zero);
    if (!heap)
        return NULL;

    struct path_walk_state st = {
        .buf = heap->buf,
        .blen = PATH_BUF_LEN,
        .dentry = dentry,
        .mnt = mnt,
        .resolved = false,
        .partial = false,
    };
    if (mnt)
        st.mp = container_of(mnt, struct mount, mnt);

    bpf_loop(PATH_WALK_MAX, walk_step, &st, 0);

    /* blen 在 bpf_loop 回调里被改写, 回到本函数已是普通 scalar,
     * 显式收窄范围后再计算长度 (verifier 需要) */
    __u32 len = 0;
    if (st.blen >= 0 && st.blen <= PATH_BUF_LEN)
        len = (__u32)(PATH_BUF_LEN - st.blen);
    if (!st.resolved || st.partial || len == 0) {
        /* map miss / 预算耗尽 → partial=false (调用方 fail-closed);
         * mnt 缺失提前终止 / 根路径 → partial=true (不表态) */
        *out_partial = st.partial || (st.resolved && len == 0);
        return NULL;
    }
    heap->buf[PATH_BUF_LEN] = '\0';   /* 路径 NUL 终止 (见函数头) */
    *out_len = len;
    *out_partial = false;
    return st.buf + st.blen;          /* blen == 写入位置偏移 */
}

#endif /* __PATH_WALK_H__ */
