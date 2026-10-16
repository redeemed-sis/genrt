#pragma once

#include <genrt/counter.h>
#include <genrt/profile.h>
#include <genrt/parse.h>
#include <stdio.h>
#include <string.h>

struct rtbench_return_sample {
    uint64_t deadline_ns, counter;
    uint32_t next_step;
};

/* Process-private BSS, touched only when explicitly profiling. No output or
 * allocation during measurement; final sleep has next_step == steps. */
static struct rtbench_return_sample profile_returns[MAX_STEPS];
static uint32_t profile_return_count;
static int profile_active;

static int profile_pair(const char *text, uint32_t *start, uint32_t *end) {
    const char *colon = strchr(text, ':');
    char left[12];
    if (colon == NULL || (size_t)(colon - text) >= sizeof(left)) return 0;
    memcpy(left, text, (size_t)(colon - text));
    left[colon - text] = 0;
    return genrt_parse_u32_decimal(left, PROFILE_PHASE_COUNT - 1, start) == 0
           && genrt_parse_u32_decimal(colon + 1, PROFILE_PHASE_COUNT - 1, end) == 0
           && *start < *end;
}

static void profile_abort(void) {
    if (profile_active) {
        genrt_profile_control(PROFILE_DISCARD, NULL);
        profile_active = 0;
    }
}

#include "rtbench_profile_report.h"
