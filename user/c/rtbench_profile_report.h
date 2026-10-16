#pragma once

/* Private rtbench reporting, called only after STOP. Read one record at a
 * time and retain fixed-size aggregates, never an additional trace snapshot. */
static const char *const profile_phase_names[PROFILE_PHASE_COUNT] = {
    "svc-entry", "validated", "timer-published", "wait-committed",
    "sleep-handoff", "irq-entry", "acknowledge", "expired-extracted",
    "scheduler-acquired", "wake-selected", "context-restored",
    "timer-rearmed", "interrupt-completed", "return-to-user",
};
static const unsigned profile_intervals[][2] = {
    {0, 1}, {1, 2}, {2, 3}, {3, 4}, {4, 5}, {5, 6}, {6, 7},
    {7, 8}, {8, 9}, {9, 10}, {10, 13}, {8, 11},
    {10, 11}, /* Direct idle wake restores context before timer rearm. */
    {11, 12}, {12, 13},
    {1, 13}, /* Early return only: no wait or interrupt occurred. */
};
enum {
    PROFILE_INTERVALS = sizeof(profile_intervals) / sizeof(*profile_intervals),
    PROFILE_DEADLINE_IRQ = PROFILE_INTERVALS,
    PROFILE_RETURN_EL0,
    PROFILE_DEADLINE_EL0,
    PROFILE_EL0_STEP,
    PROFILE_REPORT_INTERVALS,
};
struct profile_interval_stats {
    int64_t sum, maximum;
    uint32_t count;
};

enum profile_report_group {
    PROFILE_GROUP_FOCUSED,
    PROFILE_GROUP_SUBMISSION,
    PROFILE_GROUP_SUSPENDED,
    PROFILE_GROUP_TIMER_IRQ,
    PROFILE_GROUP_SELECTION,
    PROFILE_GROUP_IRQ_COMPLETION,
    PROFILE_GROUP_USER_RETURN,
    PROFILE_GROUP_SUMMARY,
    PROFILE_REPORT_GROUPS,
};

static enum profile_report_group profile_interval_group(unsigned index,
                                                         uint32_t mode, uint32_t path) {
    if (index == PROFILE_RETURN_EL0 || index == PROFILE_EL0_STEP) return PROFILE_GROUP_USER_RETURN;
    if (index >= PROFILE_INTERVALS) return PROFILE_GROUP_SUMMARY;
    if (mode == PROFILE_FOCUSED) return PROFILE_GROUP_FOCUSED;
    unsigned a = profile_intervals[index][0];
    unsigned b = profile_intervals[index][1];
    if (a < PROFILE_SLEEP_HANDOFF && b == a + 1) return PROFILE_GROUP_SUBMISSION;
    if (a == PROFILE_SLEEP_HANDOFF) return PROFILE_GROUP_SUSPENDED;
    if (a >= PROFILE_IRQ_ENTRY && a < PROFILE_SCHEDULER_ACQUIRED) return PROFILE_GROUP_TIMER_IRQ;
    if ((a == PROFILE_SCHEDULER_ACQUIRED && b == PROFILE_WAKE_SELECTED)
        || a == PROFILE_WAKE_SELECTED) return PROFILE_GROUP_SELECTION;
    if ((a == PROFILE_CONTEXT_RESTORED && b == PROFILE_TIMER_REARMED)
        || a == PROFILE_TIMER_REARMED) return PROFILE_GROUP_IRQ_COMPLETION;
    if ((path == PROFILE_WAKE_IDLE && a == PROFILE_INTERRUPT_COMPLETED)
        || (path == PROFILE_WAKE_GENERIC && a == PROFILE_CONTEXT_RESTORED
            && b == PROFILE_RETURN_TO_USER)
        || path == PROFILE_WAKE_EARLY) return PROFILE_GROUP_USER_RETURN;
    return PROFILE_GROUP_SUMMARY;
}

static int profile_read_exact(void *buffer, size_t size, size_t offset) {
    unsigned char *out = buffer;
    for (size_t done = 0; done < size;) {
        ssize_t count = genrt_profile_read(out + done, size - done, offset + done);
        if (count <= 0 || (size_t)count > size - done) return 0;
        done += (size_t)count;
    }
    return 1;
}

static int profile_ticks_ns(uint64_t ticks, uint64_t frequency, uint64_t *ns) {
    /* Architected counter frequency is 32-bit, so remainder*1e9 fits u64. */
    if (frequency == 0 || frequency > UINT32_MAX
        || ticks / frequency > UINT64_MAX / RTBENCH_NS_PER_SEC) return 0;
    return rtbench_add(ticks / frequency * RTBENCH_NS_PER_SEC,
                      ticks % frequency * RTBENCH_NS_PER_SEC / frequency, ns);
}

static int profile_deadline_ticks(uint64_t ns, uint64_t frequency, uint64_t *ticks) {
    if (frequency == 0 || frequency > UINT32_MAX
        || ns / RTBENCH_NS_PER_SEC > UINT64_MAX / frequency) return 0;
    uint64_t remainder = ns % RTBENCH_NS_PER_SEC * frequency;
    return rtbench_add(ns / RTBENCH_NS_PER_SEC * frequency,
                      remainder / RTBENCH_NS_PER_SEC
                      + (remainder % RTBENCH_NS_PER_SEC != 0), ticks);
}

static int profile_interval_add(struct profile_interval_stats *stats,
                                uint64_t start, uint64_t end, uint64_t frequency,
                                int signed_interval) {
    uint64_t magnitude;
    if ((!signed_interval && end < start)
        || !profile_ticks_ns(end >= start ? end - start : start - end,
                             frequency, &magnitude)
        || magnitude > INT64_MAX) return 0;
    int64_t ns = end >= start ? (int64_t)magnitude : -(int64_t)magnitude;
    if ((ns > 0 && stats->sum > INT64_MAX - ns)
        || (ns < 0 && stats->sum < INT64_MIN - ns)) return 0;
    stats->sum += ns;
    if (stats->count == 0 || ns > stats->maximum) stats->maximum = ns;
    stats->count++;
    return 1;
}

static int profile_report(uint64_t origin, uint64_t period, uint32_t steps,
                           const struct profile_config *config) {
    const uint64_t all = (UINT64_C(1) << PROFILE_PHASE_COUNT) - 1;
    const uint64_t early_mask = (UINT64_C(1) << PROFILE_SVC_ENTRY)
                               | (UINT64_C(1) << PROFILE_DEADLINE_VALIDATED)
                               | (UINT64_C(1) << PROFILE_RETURN_TO_USER);
    struct genrt_profile_header h;
    uint64_t span, horizon;
    if (period == 0 || !rtbench_span(period, steps, &span)
        || !rtbench_add(origin, span, &horizon)) return 0;
    if (!profile_read_exact(&h, sizeof(h), 0)
        || h.magic != GENRT_PROFILE_MAGIC || h.version != GENRT_PROFILE_VERSION
        || h.source_version != GENRT_PROFILE_SOURCE_VERSION
        || h.header_bytes != sizeof(h) || h.record_bytes != sizeof(struct genrt_profile_record)
        || h.frequency == 0 || h.frequency > UINT32_MAX
        || (h.mode != PROFILE_FULL && h.mode != PROFILE_FOCUSED)
        || h.start_phase >= h.end_phase || h.end_phase >= PROFILE_PHASE_COUNT
        || h.mode != config->mode || h.start_phase != config->start_phase
        || h.end_phase != config->end_phase || h.capacity != config->max_records
        || h.capacity != steps || steps == 0 || steps > MAX_STEPS
        || profile_return_count > steps
        || h.recorded > h.capacity || h.incomplete > h.recorded || h.early > h.recorded
        || h.lost > profile_return_count || h.recorded != profile_return_count - h.lost
        || (h.lost != 0 && h.recorded != h.capacity)) return 0;
    uint64_t selected = h.mode == PROFILE_FULL ? all
                        : (UINT64_C(1) << h.start_phase) | (UINT64_C(1) << h.end_phase);
    struct profile_interval_stats stats[3][PROFILE_REPORT_INTERVALS] = {0};
    uint64_t paths[3] = {0}, incomplete = 0, last_wait = 0;
    uint32_t last_step = 0;
    uint64_t last_counter = 0;
    uint32_t calls = 0;
    for (uint32_t step = 0; step < steps; step++) {
        if (samples[step].overrun != 0) continue;
        if (calls >= profile_return_count || profile_returns[calls].next_step != step + 1) return 0;
        calls++;
    }
    if (calls != profile_return_count) return 0;
    /* Validate all calls, including ones not retained by the kernel. */
    for (uint32_t i = 0; i < profile_return_count; i++) {
        const struct rtbench_return_sample *u = &profile_returns[i];
        uint64_t ns, expected, returned_ns;
        if (u->next_step <= last_step || u->next_step > steps
            || !rtbench_add(origin, (uint64_t)u->next_step * period, &ns)
            || ns != u->deadline_ns
            || !profile_deadline_ticks(ns, h.frequency, &expected)
            || u->counter < expected || u->counter < last_counter
            || !profile_ticks_ns(u->counter, h.frequency, &returned_ns)) return 0;
        if (u->next_step < steps) {
            uint64_t start;
            if (!rtbench_add(ns, samples[u->next_step].latency, &start)
                || start < returned_ns) return 0;
        }
        last_step = u->next_step;
        last_counter = u->counter;
    }
    for (uint64_t i = 0; i < h.recorded; i++) {
        struct genrt_profile_record r;
        size_t offset = sizeof(h) + (size_t)i * sizeof(r);
        if (!profile_read_exact(&r, sizeof(r), offset)
            || r.ordinal != i || r.thread_generation != h.owner_generation
            || r.wake_path < PROFILE_WAKE_IDLE || r.wake_path > PROFILE_WAKE_EARLY
            || (r.phase_present & ~selected) != 0) return 0;
        unsigned path = r.wake_path - 1;
        paths[path]++;
        const struct rtbench_return_sample *u = &profile_returns[i];
        uint64_t expected;
        if (!profile_deadline_ticks(u->deadline_ns, h.frequency, &expected)
            || r.requested_deadline != expected) return 0;
        for (unsigned phase = 0; phase < PROFILE_PHASE_COUNT; phase++) {
            int present = (r.phase_present & (UINT64_C(1) << phase)) != 0;
            if ((!present && r.counters[phase] != 0)
                || (present && r.counters[phase] > u->counter)
                || (r.wake_path != PROFILE_WAKE_EARLY && present
                    && phase >= PROFILE_EXPIRED_EXTRACTED
                    && r.counters[phase] < r.requested_deadline)) return 0;
        }
        uint64_t required = selected & (r.wake_path == PROFILE_WAKE_EARLY ? early_mask : all);
        if ((r.phase_present & required) != required
            || (r.wake_path != PROFILE_WAKE_EARLY && r.programmed_deadline == 0)) incomplete++;
        if (r.wake_path == PROFILE_WAKE_EARLY) {
            if (r.wait_sequence != 0 || r.programmed_deadline != 0
                || (r.phase_present & ~early_mask) != 0) return 0;
        } else {
            if (r.wait_sequence <= last_wait
                || (r.programmed_deadline != 0 && r.programmed_deadline < r.requested_deadline)) return 0;
            last_wait = r.wait_sequence;
        }
        for (unsigned j = 0; j < PROFILE_INTERVALS; j++) {
            if (h.mode == PROFILE_FOCUSED && j != 0) continue;
            unsigned a = h.mode == PROFILE_FOCUSED ? h.start_phase : profile_intervals[j][0];
            unsigned b = h.mode == PROFILE_FOCUSED ? h.end_phase : profile_intervals[j][1];
            if (h.mode == PROFILE_FULL && a == PROFILE_CONTEXT_RESTORED && b == PROFILE_TIMER_REARMED
                && r.wake_path != PROFILE_WAKE_IDLE) continue;
            if (h.mode == PROFILE_FULL && a == PROFILE_DEADLINE_VALIDATED && b == PROFILE_RETURN_TO_USER
                && r.wake_path != PROFILE_WAKE_EARLY) continue;
            uint64_t mask = (UINT64_C(1) << a) | (UINT64_C(1) << b);
            if ((r.phase_present & mask) != mask) continue;
            /* Generic selection may follow a later IRQ. Its relation to the
             * original IRQ's rearm/end branch is signed, not a causal cost. */
            int branched = r.wake_path == PROFILE_WAKE_GENERIC
                           && (a == PROFILE_WAKE_SELECTED || a == PROFILE_CONTEXT_RESTORED)
                           && (b == PROFILE_TIMER_REARMED || b == PROFILE_INTERRUPT_COMPLETED);
            if (!profile_interval_add(&stats[path][j], r.counters[a], r.counters[b],
                                      h.frequency, branched)) return 0;
        }
        if ((r.phase_present & (UINT64_C(1) << PROFILE_IRQ_ENTRY)) != 0
            && !profile_interval_add(&stats[path][PROFILE_DEADLINE_IRQ], r.requested_deadline,
                                    r.counters[PROFILE_IRQ_ENTRY], h.frequency, 1)) return 0;
        if ((r.phase_present & (UINT64_C(1) << PROFILE_RETURN_TO_USER)) != 0
            && !profile_interval_add(&stats[path][PROFILE_RETURN_EL0], r.counters[PROFILE_RETURN_TO_USER],
                                    u->counter, h.frequency, 0)) return 0;
        if (!profile_interval_add(&stats[path][PROFILE_DEADLINE_EL0], r.requested_deadline,
                                  u->counter, h.frequency, 0)) return 0;
        if (u->next_step < steps) {
            uint64_t start, returned_ns;
            if (!rtbench_add(u->deadline_ns, samples[u->next_step].latency, &start)
                || !profile_ticks_ns(u->counter, h.frequency, &returned_ns)
                || !profile_interval_add(&stats[path][PROFILE_EL0_STEP], returned_ns,
                                        start, RTBENCH_NS_PER_SEC, 0)) return 0;
        }
    }
    unsigned char eof;
    if (incomplete != h.incomplete || paths[PROFILE_WAKE_EARLY - 1] != h.early
        || genrt_profile_read(&eof, 1, sizeof(h) + (size_t)h.recorded * h.record_bytes) != 0) return 0;
    uint64_t pair_ns, write_ns;
    if (!profile_ticks_ns(h.counter_pair_ticks, h.frequency, &pair_ns)
        || !profile_ticks_ns(h.trace_write_ticks, h.frequency, &write_ns)) return 0;
    if (printf("\nSleep profile: CPU%u, %s, counter=%llu Hz\n"
               "Cycles: recorded=%llu complete=%llu incomplete=%llu lost=%llu early=%llu\n"
               "Calibration (single START observation): counter pair=%llu ns, trace write=%llu ns\n",
               h.cpu, h.mode == PROFILE_FULL ? "full" : "focused", (unsigned long long)h.frequency,
               (unsigned long long)h.recorded, (unsigned long long)(h.recorded - h.incomplete),
               (unsigned long long)h.incomplete, (unsigned long long)h.lost, (unsigned long long)h.early,
               (unsigned long long)pair_ns, (unsigned long long)write_ns) < 0) return 0;
    static const char *const names[] = {"idle", "generic", "early return"};
    static const char *const extra[] = {
        "deadline -> irq-entry (signed)", "return-to-user -> first EL0 observation",
        "deadline -> first EL0 observation", "first EL0 observation -> next STEP",
    };
    static const char *const groups[PROFILE_REPORT_GROUPS] = {
        "Selected phase pair",
        "Sleep submission",
        "Suspended interval",
        "Timer IRQ: deadline completion",
        "Resume selection",
        "Originating IRQ: timer rearm and completion",
        "Return to EL0 / next STEP",
        "Totals and deadline references (overlap stages; do not add)",
    };
    static const char *const flows[] = {
        "Flow: SVC -> block -> idle -> timer IRQ -> resume -> EL0\n",
        ("Flow: SVC -> block -> timer IRQ -> ready -> selection (possibly later) -> EL0\n"
         "IRQ completion and resume selection are separate branches, not a strict stage order.\n"),
        "Flow: SVC -> deadline already passed -> EL0 (no blocking or timer IRQ)\n",
    };
    for (unsigned path = 0; path < 3; path++) {
        if (paths[path] == 0) continue;
        if (printf("\nWake path: %s (%llu cycles)\n%s",
                   names[path], (unsigned long long)paths[path], flows[path]) < 0) return 0;
        unsigned section = 0;
        for (unsigned group = 0; group < PROFILE_REPORT_GROUPS; group++) {
            int started = 0;
            for (unsigned j = 0; j < PROFILE_REPORT_INTERVALS; j++) {
                const struct profile_interval_stats *s = &stats[path][j];
                if (s->count == 0 || profile_interval_group(j, h.mode, path + 1) != group) continue;
                if (!started) {
                    if (printf("\n%u. %s\nInterval | Count | Average (ns) | Maximum (ns)\n",
                               ++section, groups[group]) < 0) return 0;
                    started = 1;
                }
                if (j < PROFILE_INTERVALS) {
                    unsigned a = h.mode == PROFILE_FOCUSED ? h.start_phase : profile_intervals[j][0];
                    unsigned b = h.mode == PROFILE_FOCUSED ? h.end_phase : profile_intervals[j][1];
                    if (printf("%s -> %s | ", profile_phase_names[a], profile_phase_names[b]) < 0) return 0;
                } else if (printf("%s | ", extra[j - PROFILE_INTERVALS]) < 0) return 0;
                if (printf("%u | %lld | %lld\n", s->count,
                           (long long)(s->sum / s->count), (long long)s->maximum) < 0) return 0;
            }
        }
    }
    return printf("\nMissing phases and empty sections are omitted. Totals overlap stage rows.\n"
                  "Do not add totals or row maxima; counts can differ between intervals.\n"
                  "Context restoration prepares the return frame; EL0 execution starts after ERET.\n"
                  "Generic focused cross-branch intervals are signed, not elapsed costs.\n"
                  "Entry/return hooks exclude some assembly work; calibration is not subtracted.\n") >= 0;
}
