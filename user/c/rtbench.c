#include "rtbench_timing.h"
#include <genrt/parse.h>
#include <getopt.h>
#include <stdio.h>
#include <unistd.h>

/* One process-owned .bss sample per step; never allocate on the user stack. */
static struct rtbench_sample samples[MAX_STEPS];
#include "rtbench_profile.h"

static int usage(void) {
    return dprintf(STDERR_FILENO,
                   "usage: rtbench -p period_ns -n steps -o operations [-v] [-P | -F start:end]\n") < 0 ? 1 : 2;
}

static int failure(const char *reason) {
    profile_abort();
    dprintf(STDERR_FILENO, "rtbench: %s\n", reason);
    return 1;
}

static int observe(uint64_t *ns) {
    struct timespec time;
    return clock_gettime(CLOCK_MONOTONIC, &time) == 0 && rtbench_to_ns(&time, ns);
}

static uint64_t workload(uint64_t result, uint32_t operations) {
    for (uint32_t index = 0; index < operations; index++) {
        result += 1;
        /* Keep exactly one unsigned payload add per operation in a GP register. */
        __asm__ volatile("" : "+r"(result));
    }
    return result;
}

int main(int argc, char **argv) {
    uint64_t period = 0;
    uint32_t steps = 0;
    uint32_t operations = 0;
    int verbose = 0;
    unsigned seen = 0;
    struct profile_config profile = {0, 0, PROFILE_SVC_ENTRY, PROFILE_RETURN_TO_USER};
    profile_return_count = 0;
    optind = 0;
    opterr = 0;
    int option;
    while ((option = getopt(argc, argv, ":p:n:o:PF:v")) != -1) {
        unsigned bit;
        switch (option) {
        case 'p':
            bit = 1;
            if (genrt_parse_u64_decimal(optarg, UINT64_MAX, &period) != 0
                || period == 0) return usage();
            break;
        case 'n':
            bit = 2;
            if (genrt_parse_u32_decimal(optarg, MAX_STEPS, &steps) != 0
                || steps == 0) return usage();
            break;
        case 'o':
            bit = 4;
            if (genrt_parse_u32_decimal(optarg, UINT32_MAX, &operations) != 0) return usage();
            break;
        case 'P':
            bit = 8;
            profile.mode = PROFILE_FULL;
            break;
        case 'F':
            bit = 8;
            profile.mode = PROFILE_FOCUSED;
            if (!profile_pair(optarg, &profile.start_phase, &profile.end_phase)) return usage();
            break;
        case 'v':
            bit = 16;
            verbose = 1;
            break;
        default:
            return usage();
        }
        if (seen & bit) return usage();
        seen |= bit;
    }
    if ((seen & 7) != 7 || optind != argc) return usage();

    uint64_t span;
    uint64_t release;
    uint64_t horizon;
    struct timespec deadline;
    if (!rtbench_span(period, steps, &span)) return failure("period * steps overflow");
    if (profile.mode) {
        profile.max_records = steps;
        if (genrt_profile_control(PROFILE_START, &profile) != 0) return failure("profile START failed (requires wake-profile kernel)");
        profile_active = 1;
    }
    if (!observe(&release)) return failure("initial clock observation failed");
    const uint64_t origin = release;
    /* R[0] is the initial observation. R[steps] includes the final sleep,
     * not an extra sample or an extra period beyond the requested horizon. */
    if (!rtbench_add(release, span, &horizon)
        || !rtbench_to_timespec(horizon, &deadline)) return failure("final horizon overflow");

    uint64_t previous_end = release;
    uint64_t result = 0;
    for (uint32_t index = 0; index < steps; index++) {
        uint64_t next;
        uint64_t start;
        uint64_t end;
        if (!rtbench_add(release, period, &next)) return failure("release overflow");
        if (!observe(&start) || start < previous_end) return failure("start clock invalid");
        result = workload(result, operations);
        if (!observe(&end)
            || !rtbench_record(release, next, start, end, &samples[index])) return failure("end clock invalid");
        previous_end = end;
        if (samples[index].overrun == 0) {
            /* Payload end is not syscall admission: the deadline can expire
             * during submission, and a successful sleep need not block. */
            if (!rtbench_to_timespec(next, &deadline)
                || clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &deadline, NULL) != 0) return failure("absolute sleep failed");
            if (profile_active) {
                /* First practical ordered EL0 observation after the wrapper;
                 * stores and later sample bookkeeping follow this probe. */
                uint64_t returned = genrt_counter_read();
                profile_returns[profile_return_count++] = (struct rtbench_return_sample){next, returned, index + 1};
            }
        }
        /* Do not rebase or skip releases, even when several periods behind. */
        release = next;
    }

    if (profile_active && genrt_profile_control(PROFILE_STOP, NULL) != 0) return failure("profile STOP failed");

    struct rtbench_stats stats = {0};
    for (uint32_t index = 0; index < steps; index++) {
        if (!rtbench_accumulate(&stats, &samples[index])) return failure("statistics overflow");
    }
    if (verbose) {
        for (uint32_t index = 0; index < steps; index++) {
            if (printf("STEP %u release_latency_ns=%llu tet_ns=%llu overrun_ns=%llu\n",
                        index, (unsigned long long)samples[index].latency,
                        (unsigned long long)samples[index].tet,
                        (unsigned long long)samples[index].overrun) < 0) return failure("sample output failed");
        }
    }
    if (printf("SUMMARY steps=%u period_ns=%llu operations=%u "
                "release_latency_avg_ns=%llu release_latency_max_ns=%llu "
                "tet_avg_ns=%llu tet_max_ns=%llu "
                "overrun_avg_ns=%llu overrun_max_ns=%llu overruns=%u result=%llu\n",
                steps, (unsigned long long)period, operations,
                (unsigned long long)(stats.sum.latency / steps),
                (unsigned long long)stats.max.latency,
                (unsigned long long)(stats.sum.tet / steps),
                (unsigned long long)stats.max.tet,
                (unsigned long long)(stats.sum.overrun / steps),
                (unsigned long long)stats.max.overrun, stats.overruns,
                (unsigned long long)result) < 0) return failure("summary output failed");
    if (profile_active) {
        if (!profile_report(origin, period, steps, &profile)) return failure("profile report failed");
        profile_abort();
    }
    return 0;
}
