#pragma once

#include <sched.h>

#define GTRT_ESRCH 3
#define GTRT_EFAULT 14
#define GTRT_EINVAL 22

static inline int gtrt_affinity_is_only(pid_t pid, size_t expected_cpu) {
    if (expected_cpu >= CPU_SETSIZE) {
        return 0;
    }

    cpu_set_t mask;
    for (size_t index = 0; index < GENRT_CPU_MASK_WORDS; index++) {
        mask.bits[index] = ~0UL;
    }
    if (sched_getaffinity(pid, sizeof(mask), &mask) != 0) {
        return 0;
    }

    for (size_t cpu = 0; cpu < CPU_SETSIZE; cpu++) {
        if (CPU_ISSET(cpu, &mask) != (cpu == expected_cpu)) {
            return 0;
        }
    }
    return 1;
}
