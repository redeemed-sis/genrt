#include <time.h>

/* The kernel coordinator is blocked on process_join. This is the only runnable
 * non-idle thread during each sleep: every wake must retain its immutable root.
 * The coordinator checks actual MMU write counts and final resource cleanup. */
int main(void) {
    volatile unsigned long guard = 0x12345678;
    const struct timespec duration = {.tv_sec = 0, .tv_nsec = 1000000};
    for (unsigned i = 0; i < 32; i++) {
        struct timespec before, after;
        if (clock_gettime(CLOCK_MONOTONIC, &before) != 0
            || nanosleep(&duration, 0) != 0
            || clock_gettime(CLOCK_MONOTONIC, &after) != 0
            || after.tv_sec < before.tv_sec
            || (after.tv_sec == before.tv_sec
                && after.tv_nsec - before.tv_nsec < duration.tv_nsec)
            || guard != 0x12345678) return 1;
    }
    return 0;
}
