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

#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

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
}

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
    CHECK(run("10", "3", "7") == 0);
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
    CHECK(run("10", "3", "0") == 0);
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
    CHECK(run("10", "2", "0") == 0);
    CHECK(deadlines[0] == 1000000005 && deadlines[1] == 1000000015);
    CHECK(!adapter_error && !early_output && samples[1].latency == 1 && samples[1].tet == 1);

    reset();
    const uint64_t origin[] = {0, 0, 4, 13, 18, 25, 37, 39, 44};
    script(origin, 9, 2);
    CHECK(run("10", "4", "0") == 0);
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
    CHECK(run("1000", "10000", "1") == 0);
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
        CHECK(run("1000", "1", "0") == 1);
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

int main(void) {
    int (*tests[])(void) = {helpers, exact_samples, capacity, cli, failures};
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
