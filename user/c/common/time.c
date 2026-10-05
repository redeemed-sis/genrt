#include <genrt/counter.h>
#include <genrt/errno.h>
#include <stdint.h>
#include <time.h>

#define NANOSECONDS_PER_SECOND UINT64_C(1000000000)

static uint64_t clock_frequency(void) {
    /* CNTFRQ_EL0 is a 32-bit frequency. This bound also makes remainder*1e9
     * representable in uint64_t without a compiler 128-bit division helper. */
    uint64_t frequency = genrt_counter_frequency();
    return frequency <= UINT32_MAX ? frequency : 0;
}

int clock_gettime(clockid_t clockid, struct timespec *tp) {
    if (clockid != CLOCK_MONOTONIC) {
        return -GENRT_EINVAL;
    }
    if (tp == 0) {
        return -GENRT_EFAULT;
    }
    uint64_t frequency = clock_frequency();
    if (frequency == 0) {
        return -GENRT_EINVAL;
    }
    uint64_t counter = genrt_counter_read();
    uint64_t seconds = counter / frequency;
    if (seconds > INT64_MAX) {
        return -GENRT_EOVERFLOW;
    }
    struct timespec value = {
        .tv_sec = (time_t)seconds,
        .tv_nsec = (long)((counter % frequency) * NANOSECONDS_PER_SECOND
                         / frequency),
    };
    *tp = value;
    return 0;
}

int clock_getres(clockid_t clockid, struct timespec *res) {
    if (clockid != CLOCK_MONOTONIC) {
        return -GENRT_EINVAL;
    }
    uint64_t frequency = clock_frequency();
    if (frequency == 0) {
        return -GENRT_EINVAL;
    }
    if (res != 0) {
        uint64_t tick = (NANOSECONDS_PER_SECOND + frequency - 1) / frequency;
        *res = (struct timespec){
            .tv_sec = (time_t)(tick / NANOSECONDS_PER_SECOND),
            .tv_nsec = (long)(tick % NANOSECONDS_PER_SECOND),
        };
    }
    return 0;
}
