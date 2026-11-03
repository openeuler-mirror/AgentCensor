#include <linux/types.h>
#include <stdint.h>
#include <stdio.h>
#include <unistd.h>
#include "../bpf/pid_generation.h"

static int check(__u64 start_ns, __u32 user_hz)
{
    __u64 expected = (__u64)(((__uint128_t)start_ns * user_hz) / 1000000000ULL);
    __u64 actual = proc_start_ticks(start_ns, user_hz);
    if (actual != expected) {
        fprintf(stderr, "start_ns=%llu USER_HZ=%u: expected %llu, got %llu\n",
                start_ns, user_hz, expected, actual);
        return 1;
    }
    return 0;
}

int main(void)
{
    long system_hz = sysconf(_SC_CLK_TCK);
    if (system_hz <= 0 || system_hz > 1000000000L) {
        fprintf(stderr, "invalid _SC_CLK_TCK: %ld\n", system_hz);
        return 1;
    }
    const __u32 frequencies[] = {100, 250, 1000, 1024, (__u32)system_hz};
    const __u64 samples[] = {
        0, 1, 999999999ULL, 1000000000ULL, 1000000001ULL,
        86400000000000ULL, 31536000000000000ULL, UINT64_MAX
    };
    for (unsigned int i = 0; i < sizeof(frequencies) / sizeof(frequencies[0]); i++) {
        __u32 hz = frequencies[i];
        for (unsigned int j = 0; j < sizeof(samples) / sizeof(samples[0]); j++) {
            if (check(samples[j], hz))
                return 1;
        }
        for (__u64 tick = 1; tick <= 10000; tick++) {
            __u64 boundary = (tick * 1000000000ULL + hz - 1) / hz;
            if (check(boundary - 1, hz) || check(boundary, hz) || check(boundary + 1, hz))
                return 1;
        }
    }
    printf("PASS PID generation conversion: USER_HZ=100/250/1000/1024, system=%ld\n",
           system_hz);
    return 0;
}
