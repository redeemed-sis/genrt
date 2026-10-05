#pragma once

#include <stdint.h>

/* CNTKCTL_EL1 enables these read-only registers on every runtime CPU. */
static inline uint64_t genrt_counter_frequency(void) {
    uint64_t frequency;
    __asm__ volatile("mrs %0, cntfrq_el0" : "=r"(frequency));
    return frequency;
}

static inline uint64_t genrt_counter_read(void) {
    uint64_t counter;
    /* Order the sample after preceding work; the clobber also orders C loads
     * and stores around the observation used by periodic applications. */
    __asm__ volatile("isb\n\tmrs %0, cntpct_el0"
                     : "=r"(counter) : : "memory");
    return counter;
}
