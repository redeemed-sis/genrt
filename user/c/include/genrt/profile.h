#pragma once

#include <stdint.h>
#include <stddef.h>
#include <sys/types.h>
#include <genrt/errno.h>
#include "syscall.h"

/* Experimental little-endian V1 diagnostic ABI. Normal builds recognize both
 * syscalls but return -GENRT_ENOSYS. No general tracing or sampling interface. */
#define PROFILE_START 1
#define PROFILE_STOP 2
#define PROFILE_DISCARD 3
#define PROFILE_FULL 1
#define PROFILE_FOCUSED 2
#define GENRT_PROFILE_MAGIC UINT32_C(0x31505747)
#define GENRT_PROFILE_VERSION 1
#define GENRT_PROFILE_SOURCE_VERSION 1
#define GENRT_PROFILE_BUDGET (4u * 1024u * 1024u)
#define GENRT_PROFILE_MAX_RECORDS ((GENRT_PROFILE_BUDGET - 104u) / 160u)

enum genrt_profile_phase {
    PROFILE_SVC_ENTRY = 0,
    PROFILE_DEADLINE_VALIDATED,
    PROFILE_TIMER_PUBLISHED,
    PROFILE_WAIT_COMMITTED,
    PROFILE_SLEEP_HANDOFF,
    PROFILE_IRQ_ENTRY,
    PROFILE_ACKNOWLEDGE,
    PROFILE_EXPIRED_EXTRACTED,
    PROFILE_SCHEDULER_ACQUIRED,
    PROFILE_WAKE_SELECTED,
    PROFILE_CONTEXT_RESTORED,
    PROFILE_TIMER_REARMED,
    PROFILE_INTERRUPT_COMPLETED,
    PROFILE_RETURN_TO_USER,
    PROFILE_PHASE_COUNT
};

#define PROFILE_WAKE_IDLE 1
#define PROFILE_WAKE_GENERIC 2
#define PROFILE_WAKE_EARLY 3

struct profile_config {
    uint32_t mode, max_records, start_phase, end_phase;
};

struct genrt_profile_header {
    uint32_t magic, version, header_bytes, record_bytes;
    uint32_t source_version, cpu, owner_index, owner_generation;
    uint64_t frequency;
    uint32_t mode, start_phase, end_phase, capacity;
    uint64_t recorded, incomplete, lost, early;
    /* START-time calibration in raw ticks: one ordered read pair, then one
     * selected timestamp+mask write bracketed by ordered reads (includes the
     * enclosing pair overhead). These are single baseline observations, not
     * a hardware distribution or whole-profiler overhead measurement. */
    uint64_t counter_pair_ticks, trace_write_ticks;
};

struct genrt_profile_record {
    /* Zero-based eligible validated ABS call ordinal, including early calls.
     * Only the initial capacity calls are retained; subsequent calls are lost.
     * Owner index/CPU are in the header; exact generation/sequence here. */
    uint64_t ordinal;
    uint32_t thread_generation, wake_path;
    uint64_t wait_sequence, requested_deadline, programmed_deadline;
    uint64_t phase_present;
    uint64_t counters[PROFILE_PHASE_COUNT];
};

_Static_assert(sizeof(struct profile_config) == 16, "profile config ABI");
_Static_assert(sizeof(struct genrt_profile_header) == 104, "profile header ABI");
_Static_assert(offsetof(struct genrt_profile_header, frequency) == 32, "frequency ABI");
_Static_assert(offsetof(struct genrt_profile_header, recorded) == 56, "counts ABI");
_Static_assert(offsetof(struct genrt_profile_header, counter_pair_ticks) == 88, "calibration ABI");
_Static_assert(sizeof(struct genrt_profile_record) == 160, "profile record ABI");
_Static_assert(offsetof(struct genrt_profile_record, wait_sequence) == 16, "wait ABI");
_Static_assert(offsetof(struct genrt_profile_record, counters) == 48, "phase ABI");

/* One session per immutable CPU, owned by the caller's exact main ThreadId.
 * START allocates/initializes before enabling collection (1..MAX_RECORDS).
 * Full/focused both require 0 <= start < end < PHASE_COUNT. Focused retains
 * just two observations, with the same lifecycle/identity/loss accounting.
 * Feature-enabled assembly still makes one ordered entry read per live trap.
 * STOP freezes export; same-owner frozen START replaces it. DISCARD also
 * discards active storage. Foreign START returns -EBUSY; foreign STOP/read/
 * DISCARD return -EPERM. Missing STOP/read/DISCARD return -ENOENT. Owner normal
 * or fault termination frees storage; fork never inherits ownership.
 * Invalid config: -EINVAL; checked size overflow: -EOVERFLOW; allocation:
 * -ENOMEM. STOP/DISCARD ignore config. Negative errno is returned directly. */
static inline int genrt_profile_control(int command,
                                        const struct profile_config *config) {
    return (int)genrt_syscall2(SYS_PROFILE_CONTROL, command, (long)config);
}

/* Frozen header followed by recorded records, no heap snapshot. Each read is
 * clamped to MAX_USER_COPY; offset at/beyond EOF returns zero. Active read
 * returns -EBUSY, invalid nonempty destination -EFAULT. Zero-byte/EOF reads do
 * not dereference buffer. Incomplete counts records without final lifecycle
 * completion or missing required probes:
 * full blocked paths require all phases; early paths require SVC, validation,
 * return. Focused requires the selected pair intersected with that path mask.
 * Blocked paths also require a nonzero actual programmed CVAL; an earlier
 * quantum IRQ absorbing the wait without programming it is incomplete.
 * lost counts eligible calls after exhaustion. Early counts retained calls
 * that never blocked. Early records omit wait/deadline programming/IRQ probes.
 * phase_present bit N marks counter[N]; absent observations are zero. Entry
 * is after saving x0/x1; RETURN_TO_USER is before GPR restore, not actual eret.
 * Generic wake selection/restoration/return may be in a later timer/IPI; IRQ,
 * ack, extraction, rearm and completion remain those of exact wait expiry.
 * Phase IDs are indices, NOT a total timestamp ordering: generic IRQ rearm/
 * end may precede later selection/restoration/return. */
static inline ssize_t genrt_profile_read(void *buffer, size_t size,
                                         size_t offset) {
    return genrt_syscall3(SYS_PROFILE_READ, (long)buffer, (long)size, (long)offset);
}
