#include <genrt/errno.h>
#include <stdint.h>
#include <time.h>

static uint64_t frequency;
static uint64_t counter;

uint64_t genrt_counter_frequency(void) { return frequency; }
uint64_t genrt_counter_read(void) { return counter; }

int main(void) {
    const uint64_t frequencies[] = {1, 3, 19200000, 62500000, UINT32_MAX};
    const uint64_t counters[] = {0, 1, 2, 999999999, UINT32_MAX, UINT64_MAX};
    for (unsigned f = 0; f < sizeof(frequencies) / sizeof(frequencies[0]); f++) {
        frequency = frequencies[f];
        for (unsigned c = 0; c < sizeof(counters) / sizeof(counters[0]); c++) {
            counter = counters[c];
            struct timespec value = {.tv_sec = -1, .tv_nsec = -1};
            int result = clock_gettime(CLOCK_MONOTONIC, &value);
            if (counter / frequency > INT64_MAX) {
                if (result != -GENRT_EOVERFLOW || value.tv_sec != -1) return 1;
            } else if (result != 0
                       || (uint64_t)value.tv_sec != counter / frequency
                       || (uint64_t)value.tv_nsec
                              != (uint64_t)((__uint128_t)(counter % frequency)
                                            * 1000000000 / frequency)
                       || value.tv_nsec < 0 || value.tv_nsec >= 1000000000) {
                return 2;
            }
        }
        struct timespec resolution;
        if (clock_getres(CLOCK_MONOTONIC, &resolution) != 0
            || (uint64_t)resolution.tv_sec * 1000000000 + resolution.tv_nsec
                   != (1000000000 + frequency - 1) / frequency
            || clock_getres(CLOCK_MONOTONIC, 0) != 0) return 3;
    }
    struct timespec value = {.tv_sec = 42, .tv_nsec = 42};
    if (clock_gettime(77, &value) != -GENRT_EINVAL
        || clock_getres(77, &value) != -GENRT_EINVAL
        || clock_gettime(CLOCK_MONOTONIC, 0) != -GENRT_EFAULT) return 4;
    frequency = 0;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != -GENRT_EINVAL
        || clock_getres(CLOCK_MONOTONIC, 0) != -GENRT_EINVAL
        || value.tv_sec != 42 || value.tv_nsec != 42) return 5;
    frequency = UINT64_MAX;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != -GENRT_EINVAL) return 6;
    return 0;
}
