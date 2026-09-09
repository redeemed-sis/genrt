#pragma once

#include "syscall.h"

#define CPU_SETSIZE 1024
#define GENRT_CPU_MASK_BITS (8UL * sizeof(unsigned long))
#define GENRT_CPU_MASK_WORDS (CPU_SETSIZE / GENRT_CPU_MASK_BITS)

typedef struct {
    unsigned long bits[GENRT_CPU_MASK_WORDS];
} cpu_set_t;

static inline void genrt_cpu_zero(cpu_set_t *mask) {
    for (size_t index = 0; index < GENRT_CPU_MASK_WORDS; index++) {
        mask->bits[index] = 0;
    }
}

static inline void genrt_cpu_set(size_t cpu, cpu_set_t *mask) {
    if (cpu < CPU_SETSIZE) {
        mask->bits[cpu / GENRT_CPU_MASK_BITS] |=
            1UL << (cpu % GENRT_CPU_MASK_BITS);
    }
}

static inline void genrt_cpu_clear(size_t cpu, cpu_set_t *mask) {
    if (cpu < CPU_SETSIZE) {
        mask->bits[cpu / GENRT_CPU_MASK_BITS] &=
            ~(1UL << (cpu % GENRT_CPU_MASK_BITS));
    }
}

static inline int genrt_cpu_is_set(size_t cpu, const cpu_set_t *mask) {
    return cpu < CPU_SETSIZE
           && (mask->bits[cpu / GENRT_CPU_MASK_BITS]
               & (1UL << (cpu % GENRT_CPU_MASK_BITS))) != 0;
}

#define CPU_ZERO(mask) genrt_cpu_zero(mask)
#define CPU_SET(cpu, mask) genrt_cpu_set((size_t)(cpu), (mask))
#define CPU_CLR(cpu, mask) genrt_cpu_clear((size_t)(cpu), (mask))
#define CPU_ISSET(cpu, mask) genrt_cpu_is_set((size_t)(cpu), (mask))

static inline int sched_getaffinity(pid_t pid,
                                    size_t cpusetsize,
                                    cpu_set_t *mask) {
    return (int)genrt_syscall3(
        SYS_SCHED_GETAFFINITY, pid, (long)cpusetsize, (long)mask);
}
