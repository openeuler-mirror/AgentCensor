#include <errno.h>
#include <linux/bpf.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int report_failure(const char *operation)
{
    printf("%s errno=%d %s\n", operation, errno, strerror(errno));
    return 1;
}

static int probe_ptrace(pid_t target)
{
    if (ptrace(PTRACE_ATTACH, target, NULL, NULL) != 0)
        return report_failure("ptrace");
    (void)waitpid(target, NULL, 0);
    (void)ptrace(PTRACE_DETACH, target, NULL, NULL);
    puts("ptrace unexpectedly succeeded");
    return 0;
}

static int probe_traceme(void)
{
    if (ptrace(PTRACE_TRACEME, 0, NULL, NULL) != 0)
        return report_failure("traceme");
    puts("traceme unexpectedly succeeded");
    return 0;
}

static int probe_bpf(void)
{
    union bpf_attr attr = {
        .map_type = BPF_MAP_TYPE_ARRAY,
        .key_size = sizeof(unsigned int),
        .value_size = sizeof(unsigned int),
        .max_entries = 1,
    };
    int fd = (int)syscall(__NR_bpf, BPF_MAP_CREATE, &attr, sizeof(attr));
    if (fd < 0)
        return report_failure("bpf");
    close(fd);
    puts("bpf unexpectedly succeeded");
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 3 && strcmp(argv[1], "ptrace") == 0) {
        char *end = NULL;
        long target = strtol(argv[2], &end, 10);
        if (end == argv[2] || *end != '\0' || target <= 0)
            return 2;
        return probe_ptrace((pid_t)target);
    }
    if (argc == 2 && strcmp(argv[1], "traceme") == 0)
        return probe_traceme();
    if (argc == 2 && strcmp(argv[1], "bpf") == 0)
        return probe_bpf();
    fprintf(stderr, "usage: %s ptrace PID|traceme|bpf\n", argv[0]);
    return 2;
}
