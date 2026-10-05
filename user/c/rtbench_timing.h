#pragma once

#include <limits.h>
#include <stdint.h>
#include <time.h>

#define MAX_STEPS 10000U
#define RTBENCH_NS_PER_SEC 1000000000ULL

struct rtbench_sample {
    uint64_t latency;
    uint64_t tet;
    uint64_t overrun;
};

struct rtbench_stats {
    struct rtbench_sample sum;
    struct rtbench_sample max;
    uint32_t overruns;
};

/* Private portable helpers: outputs are valid only on success (return 1). */
static inline int rtbench_add(uint64_t lhs, uint64_t rhs, uint64_t *value) {
    if (rhs > UINT64_MAX - lhs) return 0;
    *value = lhs + rhs;
    return 1;
}

static inline int rtbench_span(uint64_t period, uint32_t steps, uint64_t *span) {
    if (steps == 0 || period > UINT64_MAX / steps) return 0;
    *span = period * steps;
    return 1;
}

static inline int rtbench_to_ns(const struct timespec *time, uint64_t *ns) {
    if (time->tv_sec < 0 || time->tv_nsec < 0
        || time->tv_nsec >= (long)RTBENCH_NS_PER_SEC
        || (uint64_t)time->tv_sec > UINT64_MAX / RTBENCH_NS_PER_SEC) return 0;
    return rtbench_add((uint64_t)time->tv_sec * RTBENCH_NS_PER_SEC,
                       (uint64_t)time->tv_nsec, ns);
}

static inline int rtbench_to_timespec(uint64_t ns, struct timespec *time) {
    uint64_t seconds = ns / RTBENCH_NS_PER_SEC;
    if (seconds > LONG_MAX) return 0;
    time->tv_sec = (time_t)seconds;
    time->tv_nsec = (long)(ns % RTBENCH_NS_PER_SEC);
    return 1;
}

static inline int rtbench_record(uint64_t release, uint64_t next,
                                  uint64_t start, uint64_t end,
                                  struct rtbench_sample *sample) {
    if (next < release || start < release || end < start) return 0;
    sample->latency = start - release;
    sample->tet = end - start;
    sample->overrun = end > next ? end - next : 0;
    return 1;
}

static inline int rtbench_accumulate(struct rtbench_stats *stats,
                                      const struct rtbench_sample *sample) {
    if (!rtbench_add(stats->sum.latency, sample->latency, &stats->sum.latency)
        || !rtbench_add(stats->sum.tet, sample->tet, &stats->sum.tet)
        || !rtbench_add(stats->sum.overrun, sample->overrun, &stats->sum.overrun)) return 0;
    if (sample->latency > stats->max.latency) stats->max.latency = sample->latency;
    if (sample->tet > stats->max.tet) stats->max.tet = sample->tet;
    if (sample->overrun > stats->max.overrun) stats->max.overrun = sample->overrun;
    if (sample->overrun != 0) stats->overruns++;
    return 1;
}
