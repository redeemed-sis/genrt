#define main rtbench_main
#include "../../user/c/rtbench.c"
#undef main
#include <string.h>

extern ssize_t __real_write(int fd, const void *buffer, size_t count);

static uint64_t times[32];
static size_t time_count;
static size_t observed;
static size_t slept;
static uint64_t deadlines[MAX_STEPS];
static size_t expected_observations;
static size_t expected_sleeps;
static size_t clock_fail_at;
static size_t sleep_fail_at;
static size_t malformed_at;
static struct timespec malformed_time;
static int adapter_error;
static int early_output;
static char captured[1024 * 1024];
static size_t captured_size;
static size_t stderr_size;
static char captured_error[256];
static size_t write_limit;
static size_t stdout_fail_after;
static int output_error;
static int stderr_error;
static int profiler_available, profiler_state, profiler_error;
static struct genrt_profile_header export_header;
static struct genrt_profile_record export_records[8];
static size_t profile_read_limit;
static int profile_read_error;

#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

static const char *find_text(const char *source, const char *text) {
    for (const char *at = source; *at != 0; at++) {
        if (strncmp(at, text, strlen(text)) == 0) return at;
    }
    return NULL;
}

static int contains(const char *text) {
    return find_text(captured, text) != NULL;
}

static void reset(void) {
    time_count = observed = slept = captured_size = stderr_size = 0;
    expected_observations = expected_sleeps = 0;
    clock_fail_at = sleep_fail_at = malformed_at = SIZE_MAX;
    stdout_fail_after = SIZE_MAX;
    write_limit = SIZE_MAX;
    output_error = -GENRT_EIO;
    stderr_error = adapter_error = early_output = 0;
    captured[0] = 0;
    captured_error[0] = 0;
    profiler_available = profiler_state = profiler_error = profile_active = 0;
    memset(&export_header, 0, sizeof(export_header));
    memset(export_records, 0, sizeof(export_records));
    profile_read_limit = SIZE_MAX;
    profile_read_error = 0;
}

long genrt_syscall2(long number, long command, long pointer) {
    if (number != SYS_PROFILE_CONTROL) return -GENRT_ENOSYS;
    if (!profiler_available) return -GENRT_ENOSYS;
    if (command == PROFILE_START) {
        const struct profile_config *config = (const struct profile_config *)pointer;
        export_header = (struct genrt_profile_header){
            .magic = GENRT_PROFILE_MAGIC, .version = 1, .header_bytes = 104,
            .record_bytes = 160, .capacity = config->max_records,
            .source_version = 1, .frequency = 1000000000, .owner_generation = 7,
            .mode = config->mode, .start_phase = config->start_phase,
            .end_phase = config->end_phase,
        };
        profiler_state = 1;
    } else if (command == PROFILE_STOP) {
        if (observed != expected_observations || slept != expected_sleeps) profiler_error = 1;
        if (slept > sizeof(export_records) / sizeof(*export_records)) return -GENRT_EINVAL;
        export_header.recorded = export_header.early = slept;
        for (size_t i = 0; i < slept; i++) {
            struct genrt_profile_record *r = &export_records[i];
            *r = (struct genrt_profile_record){
                .ordinal = i, .thread_generation = 7, .wake_path = PROFILE_WAKE_EARLY,
                .requested_deadline = deadlines[i],
            };
            for (unsigned phase = 0; phase < PROFILE_PHASE_COUNT; phase++) {
                if ((phase == PROFILE_SVC_ENTRY || phase == PROFILE_DEADLINE_VALIDATED
                     || phase == PROFILE_RETURN_TO_USER)
                    && (export_header.mode == PROFILE_FULL || phase == export_header.start_phase
                        || phase == export_header.end_phase)) {
                    r->phase_present |= UINT64_C(1) << phase;
                    r->counters[phase] = deadlines[i] - (phase == 0 ? 2 : phase == 1 ? 1 : 0);
                }
            }
        }
        profiler_state = 2;
    } else if (command == PROFILE_DISCARD) {
        profiler_state = 0;
    } else return -GENRT_EINVAL;
    return 0;
}

long genrt_syscall3(long number, long buffer, long size, long offset) {
    if (number != SYS_PROFILE_READ || profiler_state != 2) {
        profiler_error = 1;
        return -GENRT_EINVAL;
    }
    if (profile_read_error) return -GENRT_EIO;
    size_t total = sizeof(export_header) + (size_t)export_header.recorded * sizeof(*export_records);
    if ((size_t)offset >= total) return 0;
    size_t count = total - (size_t)offset;
    if (count > (size_t)size) count = (size_t)size;
    if (count > profile_read_limit) count = profile_read_limit;
    for (size_t i = 0; i < count; i++) {
        size_t at = (size_t)offset + i;
        ((unsigned char *)buffer)[i] = at < sizeof(export_header)
            ? ((unsigned char *)&export_header)[at]
            : ((unsigned char *)export_records)[at - sizeof(export_header)];
    }
    return (long)count;
}

uint64_t genrt_counter_read(void) { return 1000 + slept * 1000; }
uint64_t genrt_counter_frequency(void) { return 1000000000; }

/* Mock the public observation API, not its implementation/transport. */
int clock_gettime(clockid_t clock, struct timespec *out) {
    if (clock != CLOCK_MONOTONIC) {
        adapter_error = 1;
        return -GENRT_EINVAL;
    }
    size_t index = observed++;
    if (index == clock_fail_at) return -GENRT_EIO;
    if (index == malformed_at) {
        *out = malformed_time;
        return 0;
    }
    if (time_count != 0) {
        if (index >= time_count) {
            adapter_error = 1;
            return -GENRT_EIO;
        }
        return rtbench_to_timespec(times[index], out) ? 0 : -GENRT_EOVERFLOW;
    }
    /* Default: R[0]=1000, each S=R+10 and E=R+20, period=1000. */
    uint64_t ns = index == 0 ? 1000 : 1000 + ((index - 1) / 2) * 1000
                                                    + (index % 2 ? 10 : 20);
    return rtbench_to_timespec(ns, out) ? 0 : -GENRT_EOVERFLOW;
}

long genrt_syscall4(long number, long clock, long flags, long request, long remaining) {
    size_t index = slept++;
    if (number != SYS_CLOCK_NANOSLEEP || clock != CLOCK_MONOTONIC
        || flags != TIMER_ABSTIME || remaining != 0 || index >= MAX_STEPS
        || !rtbench_to_ns((const struct timespec *)request, &deadlines[index])) {
        adapter_error = 1;
        return -GENRT_EINVAL;
    }
    return index == sleep_fail_at ? -GENRT_EIO : 0;
}

ssize_t __wrap_write(int fd, const void *buffer, size_t count) {
    if (fd == STDERR_FILENO) {
        size_t available = sizeof(captured_error) - 1 - stderr_size;
        size_t copied = count < available ? count : available;
        memcpy(captured_error + stderr_size, buffer, copied);
        captured_error[stderr_size + copied] = 0;
        if (count > available) return -GENRT_EIO;
        stderr_size += count;
        return stderr_error ? -GENRT_EIO : (ssize_t)count;
    }
    if (fd != STDOUT_FILENO) return -9;
    if (observed != expected_observations || slept != expected_sleeps) early_output = 1;
    if (captured_size >= stdout_fail_after) return output_error;
    if (count > write_limit) count = write_limit;
    if (count > stdout_fail_after - captured_size) count = stdout_fail_after - captured_size;
    if (count >= sizeof(captured) - captured_size) return -GENRT_EIO;
    memcpy(captured + captured_size, buffer, count);
    captured_size += count;
    captured[captured_size] = 0;
    return (ssize_t)count;
}

static int run(const char *period, const char *steps, const char *operations) {
    char *argv[] = {"rtbench", "-p", (char *)period, "-n", (char *)steps,
                    "-o", (char *)operations, NULL};
    return rtbench_main(7, argv);
}

static int run_verbose(const char *period, const char *steps, const char *operations) {
    char *argv[] = {"rtbench", "-p", (char *)period, "-n", (char *)steps,
                    "-o", (char *)operations, "-v", NULL};
    return rtbench_main(8, argv);
}

static void script(const uint64_t *values, size_t count, size_t sleeps) {
    memcpy(times, values, count * sizeof(*values));
    time_count = expected_observations = count;
    expected_sleeps = sleeps;
}

static int exact_samples(void) {
    reset();
    const uint64_t values[] = {100, 103, 110, 113, 120, 125, 136};
    script(values, 7, 2);
    write_limit = 3;
    CHECK(run_verbose("10", "3", "7") == 0);
    CHECK(observed == 7 && slept == 2 && deadlines[0] == 110 && deadlines[1] == 120);
    CHECK(!adapter_error && !early_output && stderr_size == 0);
    CHECK(strcmp(captured,
        "STEP 0 release_latency_ns=3 tet_ns=7 overrun_ns=0\n"
        "STEP 1 release_latency_ns=3 tet_ns=7 overrun_ns=0\n"
        "STEP 2 release_latency_ns=5 tet_ns=11 overrun_ns=6\n"
        "SUMMARY steps=3 period_ns=10 operations=7 release_latency_avg_ns=3 release_latency_max_ns=5 "
        "tet_avg_ns=8 tet_max_ns=11 overrun_avg_ns=2 overrun_max_ns=6 overruns=1 result=21\n") == 0);

    reset();
    const uint64_t behind[] = {100, 101, 145, 146, 147, 148, 149};
    script(behind, 7, 0);
    CHECK(run_verbose("10", "3", "0") == 0);
    CHECK(!adapter_error && !early_output && slept == 0);
    CHECK(strcmp(captured,
        "STEP 0 release_latency_ns=1 tet_ns=44 overrun_ns=35\n"
        "STEP 1 release_latency_ns=36 tet_ns=1 overrun_ns=27\n"
        "STEP 2 release_latency_ns=28 tet_ns=1 overrun_ns=19\n"
        "SUMMARY steps=3 period_ns=10 operations=0 release_latency_avg_ns=21 release_latency_max_ns=36 "
        "tet_avg_ns=15 tet_max_ns=44 overrun_avg_ns=27 overrun_max_ns=35 overruns=3 result=0\n") == 0);

    reset();
    const uint64_t boundary[] = {999999995, 999999996, 999999997, 1000000006, 1000000007};
    script(boundary, 5, 2);
    CHECK(run_verbose("10", "2", "0") == 0);
    CHECK(deadlines[0] == 1000000005 && deadlines[1] == 1000000015);
    CHECK(!adapter_error && !early_output && samples[1].latency == 1 && samples[1].tet == 1);

    reset();
    const uint64_t origin[] = {0, 0, 4, 13, 18, 25, 37, 39, 44};
    script(origin, 9, 2);
    CHECK(run_verbose("10", "4", "0") == 0);
    CHECK(deadlines[0] == 10 && deadlines[1] == 20 && slept == 2);
    CHECK(!adapter_error && !early_output && stderr_size == 0);
    CHECK(strcmp(captured,
        "STEP 0 release_latency_ns=0 tet_ns=4 overrun_ns=0\n"
        "STEP 1 release_latency_ns=3 tet_ns=5 overrun_ns=0\n"
        "STEP 2 release_latency_ns=5 tet_ns=12 overrun_ns=7\n"
        "STEP 3 release_latency_ns=9 tet_ns=5 overrun_ns=4\n"
        "SUMMARY steps=4 period_ns=10 operations=0 release_latency_avg_ns=4 release_latency_max_ns=9 "
        "tet_avg_ns=6 tet_max_ns=12 overrun_avg_ns=2 overrun_max_ns=7 overruns=2 result=0\n") == 0);
    return 0;
}

static int capacity(void) {
    reset();
    expected_observations = 2 * MAX_STEPS + 1;
    expected_sleeps = MAX_STEPS;
    CHECK(sizeof(samples) == 240000);
    CHECK(run_verbose("1000", "10000", "1") == 0);
    CHECK(observed == expected_observations && slept == MAX_STEPS);
    CHECK(!adapter_error && !early_output && stderr_size == 0);
    size_t offset = 0;
    char row[128];
    for (uint32_t index = 0; index < MAX_STEPS; index++) {
        CHECK(deadlines[index] == 1000 + (uint64_t)(index + 1) * 1000);
        int count = snprintf(row, sizeof(row), "STEP %u release_latency_ns=10 tet_ns=10 overrun_ns=0\n", index);
        CHECK(count > 0 && (size_t)count < sizeof(row));
        CHECK(memcmp(captured + offset, row, (size_t)count) == 0);
        offset += (size_t)count;
    }
    CHECK(strcmp(captured + offset,
        "SUMMARY steps=10000 period_ns=1000 operations=1 release_latency_avg_ns=10 release_latency_max_ns=10 "
        "tet_avg_ns=10 tet_max_ns=10 overrun_avg_ns=0 overrun_max_ns=0 overruns=0 result=10000\n") == 0);
    return 0;
}

static int verbosity(void) {
    reset();
    expected_observations = 5;
    expected_sleeps = 2;
    CHECK(run("1000", "2", "7") == 0);
    CHECK(strncmp(captured, "SUMMARY steps=2 ", 16) == 0);
    CHECK(!contains("STEP 0 ") && !early_output && !adapter_error);
    char summary[512];
    CHECK(captured_size < sizeof(summary));
    memcpy(summary, captured, captured_size + 1);
    reset();
    expected_observations = 5;
    expected_sleeps = 2;
    CHECK(run_verbose("1000", "2", "7") == 0);
    CHECK(contains("STEP 0 ") && contains("STEP 1 ") && !contains("STEP 2 "));
    const char *verbose_summary = find_text(captured, "SUMMARY ");
    CHECK(verbose_summary != NULL && strcmp(verbose_summary, summary) == 0);
    CHECK(!early_output && !adapter_error);
    reset();
    expected_observations = 5;
    expected_sleeps = 2;
    char *clustered[] = {"rtbench", "-vp1000", "-n2", "-o7", NULL};
    CHECK(rtbench_main(4, clustered) == 0 && contains("STEP 0 "));
    CHECK(!early_output && !adapter_error);
    reset();
    expected_observations = 5;
    expected_sleeps = 2;
    CHECK(run("1000", "2", "7") == 0 && strcmp(captured, summary) == 0);
    reset();
    expected_observations = 3;
    expected_sleeps = 1;
    stdout_fail_after = 0;
    CHECK(run("1000", "1", "0") == 1 && !early_output);
    CHECK(strcmp(captured_error, "rtbench: summary output failed\n") == 0);
    return 0;
}

static int cli(void) {
    const char *bad_periods[] = {"", "0", "-1", "+1", " 1", "1 ", "1x", "0x10", "1.5", "18446744073709551616"};
    const char *bad_steps[] = {"", "0", "10001", "4294967296", "-1", "1x"};
    const char *bad_ops[] = {"", "4294967296", "-1", "+1", " 1", "1x"};
    for (size_t index = 0; index < sizeof(bad_periods) / sizeof(*bad_periods); index++) {
        reset();
        CHECK(run(bad_periods[index], "1", "0") == 2 && observed == 0 && slept == 0);
    }
    for (size_t index = 0; index < sizeof(bad_steps) / sizeof(*bad_steps); index++) {
        reset();
        CHECK(run("1", bad_steps[index], "0") == 2 && observed == 0);
    }
    for (size_t index = 0; index < sizeof(bad_ops) / sizeof(*bad_ops); index++) {
        reset();
        CHECK(run("1", "1", bad_ops[index]) == 2 && observed == 0);
    }
    char *invalid[][10] = {
        {"rtbench", NULL},
        {"rtbench", "-p", "1", "-n", "1", NULL},
        {"rtbench", "-p", "1", "-o", "0", NULL},
        {"rtbench", "-n", "1", "-o", "0", NULL},
        {"rtbench", "-p", "1", "-n", "1", "-o", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "-p2", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "-n2", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "-o2", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "-x", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "extra", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "--", "extra", NULL},
        {"rtbench", "--period", "1", "-n1", "-o0", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "-c1", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "-P", "-c1", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "-F5:8", "-c", "1", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "-vv", NULL},
        {"rtbench", "-p1", "-n1", "-o0", "-v1", NULL},
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(*invalid); index++) {
        reset();
        int argc = 0;
        while (invalid[index][argc]) argc++;
        CHECK(rtbench_main(argc, invalid[index]) == 2);
        CHECK(observed == 0 && slept == 0 && captured_size == 0 && stderr_size > 0);
    }
    reset();
    expected_observations = 3;
    expected_sleeps = 1;
    char *attached[] = {"rtbench", "-o00", "-n01", "-p01000", "--", NULL};
    CHECK(rtbench_main(5, attached) == 0 && !adapter_error && !early_output);
    /* Accept the full u32 operations range without executing billions of adds. */
    reset();
    clock_fail_at = 0;
    CHECK(run("1000", "1", "4294967295") == 1 && observed == 1);
    reset();
    clock_fail_at = 0;
    CHECK(run("18446744073709551615", "1", "0") == 1 && observed == 1);
    return 0;
}

static int failures(void) {
    reset();
    CHECK(run("18446744073709551615", "2", "0") == 1 && observed == 0);
    reset();
    CHECK(run("18446744073709551615", "1", "0") == 1 && observed == 1);
    for (size_t index = 0; index < 3; index++) {
        reset();
        clock_fail_at = index;
        CHECK(run("1000", "1", "0") == 1 && captured_size == 0);
    }
    for (size_t index = 0; index < 2; index++) {
        reset();
        sleep_fail_at = index;
        CHECK(run("1000", "2", "0") == 1 && slept == index + 1 && captured_size == 0);
    }
    const struct timespec invalid[] = {
        {-1, 0}, {0, -1}, {0, 1000000000L}, {LONG_MAX, 0},
        {18446744073L, 709551616L},
    };
    for (size_t index = 0; index < sizeof(invalid) / sizeof(*invalid); index++) {
        for (size_t call = 0; call < 3; call++) {
            reset();
            malformed_at = call;
            malformed_time = invalid[index];
            CHECK(run("1000", "1", "0") == 1 && captured_size == 0);
        }
    }
    const uint64_t backwards[][5] = {
        {100, 99, 100, 110, 110},
        {100, 101, 100, 110, 110},
        {100, 101, 125, 124, 125},
        {100, 101, 102, 109, 110},
    };
    for (size_t index = 0; index < sizeof(backwards) / sizeof(*backwards); index++) {
        reset();
        script(backwards[index], 5, 0);
        CHECK(run("10", "2", "0") == 1 && captured_size == 0);
    }
    /* Large valid individual observations produce a checked sum overflow. */
    reset();
    const uint64_t overflow[] = {0, UINT64_MAX / 2 + 10, UINT64_MAX / 2 + 11,
                                  UINT64_MAX / 2 + 12, UINT64_MAX / 2 + 13};
    script(overflow, 5, 0);
    CHECK(run("1", "2", "0") == 1 && observed == 5 && captured_size == 0);
    CHECK(strcmp(captured_error, "rtbench: statistics overflow\n") == 0);
    for (size_t index = 0; index < 4; index++) {
        reset();
        expected_observations = 3;
        expected_sleeps = 1;
        stdout_fail_after = index == 2 ? strlen("STEP 0 release_latency_ns=10 tet_ns=10 overrun_ns=0\n") : 0;
        if (index == 3) stdout_fail_after = 7;
        output_error = index == 1 ? 0 : -GENRT_EIO;
        CHECK(run_verbose("1000", "1", "0") == 1);
        CHECK(!adapter_error && !early_output && stderr_size > 0);
    }
    reset();
    stderr_error = 1;
    CHECK(run("0", "1", "0") == 1 && observed == 0);
    return 0;
}

static int helpers(void) {
    uint64_t ns;
    struct timespec time;
    CHECK(rtbench_to_timespec(UINT64_MAX, &time));
    CHECK(rtbench_to_ns(&time, &ns) && ns == UINT64_MAX);
    CHECK(!rtbench_add(UINT64_MAX, 1, &ns));
    CHECK(rtbench_add(UINT64_MAX - 1, 1, &ns) && ns == UINT64_MAX);
    CHECK(!rtbench_span(1, 0, &ns));
    CHECK(rtbench_span(UINT64_MAX, 1, &ns) && ns == UINT64_MAX);
    CHECK(!rtbench_span(UINT64_MAX / 2 + 1, 2, &ns));
    struct rtbench_sample sample;
    CHECK(!rtbench_record(10, 9, 10, 10, &sample));
    CHECK(rtbench_record(10, 20, 10, 20, &sample) && sample.overrun == 0);
    CHECK(rtbench_record(10, 20, 10, 21, &sample) && sample.overrun == 1);
    for (size_t index = 0; index < 3; index++) {
        struct rtbench_stats stats = {0};
        struct rtbench_sample huge = {0};
        if (index == 0) huge.latency = UINT64_MAX;
        if (index == 1) huge.tet = UINT64_MAX;
        if (index == 2) huge.overrun = UINT64_MAX;
        CHECK(rtbench_accumulate(&stats, &huge));
        CHECK(!rtbench_accumulate(&stats, &huge));
    }
    CHECK(workload(0, 0) == 0 && workload(7, 123) == 130);
    return 0;
}

static int profiling(void) {
    char *argv[] = {"rtbench", "-p1000", "-n2", "-o0", "-P", NULL};
    reset();
    CHECK(rtbench_main(5, argv) == 1 && observed == 0);
    CHECK(strcmp(captured_error, "rtbench: profile START failed (requires wake-profile kernel)\n") == 0);
    reset();
    profiler_available = 1;
    profile_read_limit = 3;
    expected_observations = 5; expected_sleeps = 2;
    CHECK(rtbench_main(5, argv) == 0);
    CHECK(export_header.capacity == 2 && profile_return_count == 2);
    CHECK(!early_output && !profiler_error && profiler_state == 0);
    CHECK(contains("Sleep profile: CPU0, full, counter=1000000000 Hz\n"));
    CHECK(contains("Cycles: recorded=2 complete=2 incomplete=0 lost=0 early=2\n"));
    CHECK(contains("svc-entry -> validated | 2 | 1 | 1\n"));
    CHECK(!contains("PROFILE_"));
    CHECK(!contains("STEP 0 "));
    reset();
    profiler_available = 1;
    expected_observations = 7; expected_sleeps = 3;
    char *focused[] = {"rtbench", "-F5:8", "-p1000", "-n3", "-o0", NULL};
    CHECK(rtbench_main(5, focused) == 0);
    CHECK(export_header.capacity == 3 && profile_return_count == 3);
    CHECK(!early_output && !profiler_error && profiler_state == 0);
    CHECK(contains("Sleep profile: CPU0, focused,"));
    CHECK(!contains("irq-entry -> scheduler-acquired"));
    CHECK(!contains("PROFILE_"));
    reset();
    profiler_available = 1;
    expected_observations = 5; expected_sleeps = 2;
    char *verbose[] = {"rtbench", "-p1000", "-n2", "-o0", "-P", "-v", NULL};
    CHECK(rtbench_main(6, verbose) == 0);
    CHECK(contains("STEP 0 ") && contains("STEP 1 ") && contains("Sleep profile:"));
    CHECK(!early_output && !profiler_error && profiler_state == 0);
    /* The largest step count is also a valid profile capacity in both modes.
     * Fail the first clock read to avoid emitting the maximum-size export. */
    char *modes[] = {"-P", "-F5:8"};
    for (size_t index = 0; index < sizeof(modes) / sizeof(*modes); index++) {
        reset();
        profiler_available = 1;
        clock_fail_at = 0;
        char *maximum[] = {"rtbench", "-p1000", "-n10000", "-o0", modes[index], NULL};
        CHECK(rtbench_main(5, maximum) == 1 && observed == 1);
        CHECK(export_header.capacity == MAX_STEPS && profiler_state == 0);
        CHECK(!profiler_error && profile_return_count == 0);
    }
    reset();
    CHECK(!profile_pair("2:2", &export_header.start_phase, &export_header.end_phase));
    CHECK(!profile_pair("0:14", &export_header.start_phase, &export_header.end_phase));
    CHECK(!profile_pair("1:2:3", &export_header.start_phase, &export_header.end_phase));
    CHECK(profile_pair("5:8", &export_header.start_phase, &export_header.end_phase));
    return 0;
}

/* One idle wake, one delayed generic wake, and one already-past deadline.
 * Generic IRQ completion precedes selection; final sleep has no next STEP. */
static struct profile_config report_fixture(uint32_t mode, uint32_t start, uint32_t end) {
    reset();
    profiler_state = 2;
    memset(samples, 0, sizeof(samples));
    profile_return_count = 3;
    export_header = (struct genrt_profile_header){
        .magic = GENRT_PROFILE_MAGIC, .version = GENRT_PROFILE_VERSION,
        .source_version = GENRT_PROFILE_SOURCE_VERSION, .header_bytes = 104,
        .record_bytes = 160, .cpu = 3, .owner_generation = 7,
        .frequency = 1000000000, .mode = mode, .start_phase = start, .end_phase = end,
        .capacity = 4, .recorded = 3, .early = 1,
    };
    const uint64_t counters[][PROFILE_PHASE_COUNT] = {
        {1800,1810,1820,1830,1840,2000,2010,2020,2030,2040,2050,2060,2070,2080},
        {2800,2810,2820,2830,2840,3000,3010,3020,3030,3100,3110,3040,3050,3120},
        {5010,5020,0,0,0,0,0,0,0,0,0,0,0,5030},
    };
    const uint64_t deadlines[] = {2000, 3000, 5000};
    const uint64_t returns[] = {2100, 3140, 5040};
    const uint32_t next_steps[] = {1, 2, 4};
    for (unsigned i = 0; i < 3; i++) {
        struct genrt_profile_record *r = &export_records[i];
        *r = (struct genrt_profile_record){
            .ordinal = i, .thread_generation = 7, .wake_path = i + 1,
            .wait_sequence = i == 2 ? 0 : i + 1,
            .requested_deadline = deadlines[i], .programmed_deadline = i == 2 ? 0 : deadlines[i],
        };
        for (unsigned p = 0; p < PROFILE_PHASE_COUNT; p++) {
            if (counters[i][p] != 0 && (mode == PROFILE_FULL || p == start || p == end)) {
                r->phase_present |= UINT64_C(1) << p;
                r->counters[p] = counters[i][p];
            }
        }
        profile_returns[i] = (struct rtbench_return_sample){deadlines[i], returns[i], next_steps[i]};
    }
    samples[1].latency = samples[2].latency = 200;
    samples[2].overrun = 1;
    return (struct profile_config){mode, 4, start, end};
}

static int profile_reports(void) {
    struct profile_config c = report_fixture(PROFILE_FULL, 0, 13);
    write_limit = profile_read_limit = 3;
    CHECK(profile_report(1000, 1000, 4, &c));
    CHECK(contains("recorded=3 complete=3 incomplete=0 lost=0 early=1\n"));
    CHECK(contains("Wake path: idle (1 cycles)"));
    CHECK(contains("Wake path: generic (1 cycles)"));
    CHECK(contains("Wake path: early return (1 cycles)"));
    CHECK(contains("scheduler-acquired -> wake-selected | 1 | 70 | 70\n"));
    CHECK(contains("first EL0 observation -> next STEP | 1 | 60 | 60\n"));
    CHECK(contains("validated -> return-to-user | 1 | 10 | 10\n"));
    CHECK(!contains("PROFILE_") && !early_output && !profiler_error);

    c = report_fixture(PROFILE_FOCUSED, 9, 11);
    CHECK(profile_report(1000, 1000, 4, &c));
    CHECK(contains("wake-selected -> timer-rearmed | 1 | -60 | -60\n"));
    CHECK(!contains("svc-entry -> validated"));
    c = report_fixture(PROFILE_FOCUSED, 5, 8);
    CHECK(profile_report(1000, 1000, 4, &c));
    CHECK(contains("irq-entry -> scheduler-acquired | 1 | 30 | 30\n"));

    c = report_fixture(PROFILE_FULL, 0, 13);
    export_records[0].phase_present &= ~(UINT64_C(1) << PROFILE_TIMER_PUBLISHED);
    export_records[0].counters[PROFILE_TIMER_PUBLISHED] = 0;
    export_header.incomplete = 1;
    CHECK(profile_report(1000, 1000, 4, &c));
    CHECK(contains("complete=2 incomplete=1"));
    CHECK(!contains("validated -> timer-published | 1 | 0 | 0"));

    for (unsigned defect = 0; defect < 12; defect++) {
        c = report_fixture(PROFILE_FULL, 0, 13);
        switch (defect) {
        case 0: export_header.source_version++; break;
        case 1: export_header.frequency = 0; break;
        case 2: export_header.incomplete = 1; break;
        case 3: export_records[0].thread_generation++; break;
        case 4: export_records[1].wait_sequence = 1; break;
        case 5: export_records[0].counters[PROFILE_ACKNOWLEDGE] = 1999; break;
        case 6: export_records[0].requested_deadline++; break;
        case 7: export_records[0].counters[PROFILE_RETURN_TO_USER] = 2200; break;
        case 8: profile_read_limit = 0; break;
        case 9: profile_read_error = 1; break;
        case 10: export_records[2].phase_present |= UINT64_C(1) << PROFILE_IRQ_ENTRY; break;
        case 11: export_header.recorded = 5; break;
        }
        CHECK(!profile_report(1000, 1000, 4, &c) && captured_size == 0);
    }
    c = report_fixture(PROFILE_FULL, 0, 13);
    stdout_fail_after = 0;
    CHECK(!profile_report(1000, 1000, 4, &c));

    uint64_t ns, ticks;
    CHECK(profile_ticks_ns(1, 19200000, &ns) && ns == 52);
    CHECK(profile_deadline_ticks(53, 19200000, &ticks) && ticks == 2);
    CHECK(!profile_ticks_ns(UINT64_MAX, 1, &ns));
    CHECK(!profile_deadline_ticks(UINT64_MAX, UINT32_MAX, &ticks));
    struct profile_interval_stats s = {0};
    CHECK(profile_interval_add(&s, 0, 12, 1000000000, 0));
    CHECK(profile_interval_add(&s, 0, 20, 1000000000, 0));
    CHECK(s.sum / s.count == 16 && s.maximum == 20);
    s = (struct profile_interval_stats){0};
    CHECK(profile_interval_add(&s, 20, 0, 1000000000, 1));
    CHECK(profile_interval_add(&s, 10, 0, 1000000000, 1));
    CHECK(s.sum / s.count == -15 && s.maximum == -10);
    CHECK(!profile_interval_add(&s, 10, 0, 1000000000, 0));
    s.sum = INT64_MAX;
    CHECK(!profile_interval_add(&s, 0, 1, 1000000000, 0));
    s.sum = INT64_MIN;
    CHECK(!profile_interval_add(&s, 1, 0, 1000000000, 1));
    return 0;
}

static int profile_report_groups(void) {
    struct profile_config c = report_fixture(PROFILE_FULL, 0, 13);
    CHECK(profile_report(1000, 1000, 4, &c));
    const char *idle = find_text(captured, "Wake path: idle");
    const char *generic = find_text(captured, "Wake path: generic");
    const char *early = find_text(captured, "Wake path: early return");
    CHECK(idle != NULL && generic != NULL && early != NULL && idle < generic && generic < early);
    const char *sections[] = {
        "1. Sleep submission\n", "2. Suspended interval\n",
        "3. Timer IRQ: deadline completion\n", "4. Resume selection\n",
        "5. Originating IRQ: timer rearm and completion\n",
        "6. Return to EL0 / next STEP\n",
        "7. Totals and deadline references (overlap stages; do not add)\n",
    };
    const char *at = idle;
    for (size_t i = 0; i < sizeof(sections) / sizeof(*sections); i++) {
        const char *next = find_text(at, sections[i]);
        CHECK(next != NULL && next < generic);
        at = next + strlen(sections[i]);
    }
    const char *rearm = find_text(idle, "context-restored -> timer-rearmed |");
    const char *end = find_text(idle, "timer-rearmed -> interrupt-completed |");
    const char *ret = find_text(idle, "interrupt-completed -> return-to-user |");
    const char *total = find_text(idle, sections[6]);
    const char *overlap = find_text(idle, "context-restored -> return-to-user |");
    CHECK(rearm != NULL && end != NULL && ret != NULL && total != NULL && overlap != NULL);
    CHECK(rearm < end && end < ret && ret < total && total < overlap && overlap < generic);
    const char *generic_return = find_text(generic, "context-restored -> return-to-user |");
    const char *generic_total = find_text(generic, sections[6]);
    const char *generic_overlap = find_text(generic, "interrupt-completed -> return-to-user |");
    CHECK(generic_return != NULL && generic_total != NULL && generic_overlap != NULL);
    CHECK(generic_return < generic_total && generic_total < generic_overlap && generic_overlap < early);
    CHECK(find_text(early, "2. Return to EL0 / next STEP\n") != NULL);
    CHECK(find_text(early, "3. Totals and deadline references") != NULL);
    CHECK(find_text(early, "Timer IRQ: deadline completion") == NULL);
    CHECK(contains("IRQ completion and resume selection are separate branches"));

    c = report_fixture(PROFILE_FOCUSED, 9, 11);
    CHECK(profile_report(1000, 1000, 4, &c));
    CHECK(contains("1. Selected phase pair\n"));
    CHECK(contains("2. Return to EL0 / next STEP\n"));
    CHECK(contains("3. Totals and deadline references"));
    CHECK(contains("wake-selected -> timer-rearmed | 1 | -60 | -60\n"));
    CHECK(!contains("Sleep submission\n") && !contains("Suspended interval\n"));
    return 0;
}

int main(void) {
    int (*tests[])(void) = {helpers, exact_samples, capacity, verbosity, cli, failures,
                          profiling, profile_reports, profile_report_groups};
    for (size_t index = 0; index < sizeof(tests) / sizeof(*tests); index++) {
        int line = tests[index]();
        if (line != 0) {
            char error[80];
            int count = snprintf(error, sizeof(error), "rtbench host test %zu failed at line %d\n", index, line);
            if (count > 0) __real_write(STDERR_FILENO, error, (size_t)count);
            return 1;
        }
    }
    return 0;
}
