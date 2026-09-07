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

static inline int clock_gettime(clockid_t clockid, struct timespec *tp) {
    return (int)genrt_syscall2(SYS_CLOCK_GETTIME, clockid, (long)tp);
}

static inline int clock_getres(clockid_t clockid, struct timespec *res) {
    return (int)genrt_syscall2(SYS_CLOCK_GETRES, clockid, (long)res);
}

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
