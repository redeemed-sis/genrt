#pragma once

#include "syscall.h"

typedef long time_t;
typedef int clockid_t;

struct timespec {
    time_t tv_sec;
    long tv_nsec;
};

#define CLOCK_MONOTONIC 1
#define TIMER_ABSTIME 1

/* Counter-backed EL0 operations. Non-NULL output pointers must be writable;
 * use the raw syscall ABI when kernel pointer validation is required. */
int clock_gettime(clockid_t clockid, struct timespec *tp);
int clock_getres(clockid_t clockid, struct timespec *res);

static inline int clock_nanosleep(clockid_t clockid, int flags,
                                  const struct timespec *req,
                                  struct timespec *rem) {
    return (int)genrt_syscall4(SYS_CLOCK_NANOSLEEP,
                              clockid,
                              flags,
                              (long)req,
                              (long)rem);
}

static inline int nanosleep(const struct timespec *req, struct timespec *rem) {
    return clock_nanosleep(CLOCK_MONOTONIC, 0, req, rem);
}
