#pragma once

/* Test-only sleep transport. Observation is mocked at the public API boundary.
 * These are existing ABI numbers, not a hosted libc time replacement. */
#define SYS_CLOCK_GETTIME 14
#define SYS_CLOCK_GETRES 15
#define SYS_CLOCK_NANOSLEEP 16

long genrt_syscall2(long number, long clock, long time);
long genrt_syscall4(long number, long clock, long flags, long request, long remaining);
