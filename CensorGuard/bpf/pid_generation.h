#ifndef CENSORGUARD_PID_GENERATION_H
#define CENSORGUARD_PID_GENERATION_H

/* Splitting seconds and remainder avoids overflowing start_ns * user_hz. */
static __inline__ __attribute__((__always_inline__))
__u64 proc_start_ticks(__u64 start_ns, __u32 user_hz)
{
    return (start_ns / 1000000000ULL) * user_hz +
           ((start_ns % 1000000000ULL) * user_hz) / 1000000000ULL;
}

#endif
