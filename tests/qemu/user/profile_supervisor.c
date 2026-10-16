#include "affinity.h"
#include "protocol.h"
#include <genrt/profile.h>
#include <time.h>
#include <string.h>

static const char *producer = "profile-supervisor";
static unsigned char export_buffer[8192];
#define CHECK(x) do { if (!(x)) { dprintf(STDERR_FILENO,"profile assertion line %d\n",__LINE__); return __LINE__ % 120 + 1; } } while (0)
#define FULL_MASK ((UINT64_C(1) << PROFILE_PHASE_COUNT) - 1)

static struct profile_config config(uint32_t mode, uint32_t count) {
    return (struct profile_config){mode, count, PROFILE_SVC_ENTRY, PROFILE_RETURN_TO_USER};
}
static uint64_t now_ns(void) {
    struct timespec time;
    if (clock_gettime(CLOCK_MONOTONIC, &time) != 0) exit(120);
    return (uint64_t)time.tv_sec * 1000000000 + (uint64_t)time.tv_nsec;
}
static int sleep_until(uint64_t deadline) {
    struct timespec time = {(long)(deadline / 1000000000), (long)(deadline % 1000000000)};
    return clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &time, NULL);
}
static int absolute_sleep(uint64_t delta) { return sleep_until(now_ns() + delta); }
static int wait_exit(pid_t child, int expected) {
    int status;
    return child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status)
           && WEXITSTATUS(status) == expected;
}
static int header(struct genrt_profile_header *out) {
    return genrt_profile_read(out, sizeof(*out), 0) == sizeof(*out)
        && out->magic == GENRT_PROFILE_MAGIC && out->version == 1
        && out->source_version == 1 && out->header_bytes == 104 && out->record_bytes == 160
        && out->frequency != 0;
}
static int record(struct genrt_profile_record *out, size_t index) {
    return genrt_profile_read(out, sizeof(*out), sizeof(struct genrt_profile_header) + index * sizeof(*out)) == sizeof(*out);
}

/* CPU0 exercises actual safe-copy and configuration failures, not injection. */
static int validation(void) {
    struct profile_config c = config(PROFILE_FULL, 1);
    CHECK(genrt_profile_control(PROFILE_STOP, NULL) == -GENRT_ENOENT);
    CHECK(genrt_profile_read(export_buffer, 1, 0) == -GENRT_ENOENT);
    CHECK(genrt_profile_control(999, NULL) == -GENRT_EINVAL);
    CHECK(genrt_profile_control(PROFILE_START, NULL) == -GENRT_EFAULT);
    c.max_records = 0;
    CHECK(genrt_profile_control(PROFILE_START, &c) == -GENRT_EINVAL);
    c.max_records = GENRT_PROFILE_MAX_RECORDS + 1;
    CHECK(genrt_profile_control(PROFILE_START, &c) == -GENRT_EINVAL);
    c = config(0, 1);
    CHECK(genrt_profile_control(PROFILE_START, &c) == -GENRT_EINVAL);
    c = config(PROFILE_FULL, 1); c.end_phase = c.start_phase;
    CHECK(genrt_profile_control(PROFILE_START, &c) == -GENRT_EINVAL);
    c = config(PROFILE_FULL, 1);
    CHECK(genrt_profile_control(PROFILE_START, &c) == 0);
    CHECK(genrt_profile_control(PROFILE_START, &c) == -GENRT_EBUSY);
    CHECK(genrt_profile_read(export_buffer, 1, 0) == -GENRT_EBUSY);
    CHECK(genrt_profile_control(PROFILE_STOP, NULL) == 0);
    CHECK(genrt_profile_read((void *)1, 1, 0) == -GENRT_EFAULT);
    CHECK(genrt_profile_read(NULL, 0, 0) == 0);
    CHECK(genrt_profile_read((void *)1, 1, SIZE_MAX) == 0);
    CHECK(genrt_profile_control(PROFILE_DISCARD, NULL) == 0);
    return 0;
}

/* Three consecutive owner-local episodes must keep unique wait sequences and
 * all phases. No other process is runnable on CPU3, forcing idle selection. */
static int idle_trace(void) {
    struct profile_config c = config(PROFILE_FULL, 3);
    struct genrt_profile_header h;
    struct genrt_profile_record r;
    CHECK(gtrt_affinity_is_only(0, 3));
    CHECK(genrt_profile_control(PROFILE_START, &c) == 0);
    for (int i = 0; i < 3; i++) CHECK(absolute_sleep(5000000) == 0);
    CHECK(genrt_profile_control(PROFILE_STOP, NULL) == 0);
    CHECK(header(&h) && h.cpu == 3 && h.recorded == 3 && h.incomplete == 0 && h.lost == 0 && h.early == 0);
    uint64_t previous_wait = 0;
    for (size_t i = 0; i < 3; i++) {
        CHECK(record(&r,i) && r.ordinal == i && r.thread_generation == h.owner_generation
              && r.wait_sequence > previous_wait && r.wake_path == PROFILE_WAKE_IDLE
              && r.phase_present == FULL_MASK && r.programmed_deadline >= r.requested_deadline);
        for (size_t j = 1; j < PROFILE_PHASE_COUNT; j++) CHECK(r.counters[j] >= r.counters[j-1]);
        previous_wait = r.wait_sequence;
    }
    return 0; /* Normal exit must reclaim a frozen session. */
}

/* Only the selected pair is retained. One retained early call has no token;
 * exhaustion counts every later call without overwriting the first two. */
static int focused_loss(void) {
    struct profile_config c = config(PROFILE_FOCUSED, 2);
    c.start_phase = PROFILE_DEADLINE_VALIDATED;
    c.end_phase = PROFILE_RETURN_TO_USER;
    CHECK(genrt_profile_control(PROFILE_START, &c) == 0);
    CHECK(absolute_sleep(5000000) == 0);
    struct timespec past = {0,0};
    CHECK(clock_nanosleep(CLOCK_MONOTONIC,TIMER_ABSTIME,&past,NULL) == 0);
    for (int i = 0; i < 3; i++) CHECK(absolute_sleep(5000000) == 0);
    CHECK(genrt_profile_control(PROFILE_STOP,NULL) == 0);
    struct genrt_profile_header h;
    struct genrt_profile_record r;
    CHECK(header(&h) && h.recorded == 2 && h.lost == 3 && h.incomplete == 0 && h.early == 1);
    for (int i = 0; i < 2; i++) {
        CHECK(record(&r,i) && r.phase_present == ((UINT64_C(1)<<1)|(UINT64_C(1)<<13)));
        for (int j = 0; j < PROFILE_PHASE_COUNT; j++) CHECK(j == 1 || j == 13 || r.counters[j] == 0);
    }
    CHECK(r.wake_path == PROFILE_WAKE_EARLY && r.wait_sequence == 0);
    CHECK(genrt_profile_control(PROFILE_START,&c) == 0); /* Frozen replacement. */
    CHECK(genrt_profile_control(PROFILE_DISCARD,NULL) == 0);
    return 0;
}

/* The peer is still current when the one-ms deadline expires. It later exits,
 * blocks, or yields via its RR deadline, exercising eventual resume branches. */
static int generic_trace(int method) {
    pid_t peer = fork();
    CHECK(peer >= 0);
    if (peer == 0) {
        uint64_t end = now_ns() + (method == 0 ? 50000000 : 3000000);
        while (now_ns() < end) __asm__ volatile("" ::: "memory");
        if (method == 2 && absolute_sleep(5000000) != 0) exit(121);
        exit(0);
    }
    struct profile_config c = config(PROFILE_FULL,1);
    CHECK(genrt_profile_control(PROFILE_START,&c) == 0);
    CHECK(absolute_sleep(1000000) == 0);
    CHECK(genrt_profile_control(PROFILE_STOP,NULL) == 0);
    struct genrt_profile_header h;
    struct genrt_profile_record r;
    CHECK(header(&h) && h.recorded == 1 && h.incomplete == 0);
    CHECK(record(&r,0) && r.wake_path == PROFILE_WAKE_GENERIC && r.phase_present == FULL_MASK);
    CHECK(r.counters[5] <= r.counters[6] && r.counters[6] <= r.counters[7]
        && r.counters[7] <= r.counters[8] && r.counters[8] <= r.counters[9]
        && r.counters[9] <= r.counters[10] && r.counters[10] <= r.counters[13]
        && r.counters[11] <= r.counters[12] && r.counters[12] <= r.counters[13]);
    CHECK(wait_exit(peer,0));
    return 0;
}
static int generic_rr(void) { return generic_trace(0); }
static int generic_exit(void) { return generic_trace(1); }
static int generic_block(void) { return generic_trace(2); }

/* Reads straddle both fixed layouts and the safe-copy ceiling. A parent-owned
 * frozen session remains inaccessible to a same-CPU fork child. */
static int chunks_and_owner(void) {
    struct profile_config c = config(PROFILE_FULL,40);
    CHECK(genrt_profile_control(PROFILE_START,&c) == 0);
    struct timespec past = {0,0};
    for (int i = 0; i < 40; i++) CHECK(clock_nanosleep(CLOCK_MONOTONIC,TIMER_ABSTIME,&past,NULL) == 0);
    CHECK(genrt_profile_control(PROFILE_STOP,NULL) == 0);
    CHECK(genrt_profile_read(export_buffer,sizeof(export_buffer),0) == 1024);
    unsigned char one;
    for (size_t i = 0; i < 1024; i++) {
        CHECK(genrt_profile_read(&one,1,i) == 1 && one == export_buffer[i]);
    }
    pid_t child = fork(); CHECK(child >= 0);
    if (child == 0) {
        int ok = genrt_profile_control(PROFILE_START,&c) == -GENRT_EBUSY
            && genrt_profile_control(PROFILE_STOP,NULL) == -GENRT_EPERM
            && genrt_profile_control(PROFILE_DISCARD,NULL) == -GENRT_EPERM
            && genrt_profile_read(&one,1,0) == -GENRT_EPERM;
        exit(ok ? 0 : 122);
    }
    CHECK(wait_exit(child,0));
    CHECK(genrt_profile_control(PROFILE_DISCARD,NULL) == 0);
    return 0;
}

/* Three foreign CPU owners retain maximal buffers while CPU0 requests a
 * fourth. The four bounded budgets exceed the real 16-MiB heap with runtime
 * owners; at least one request must return ENOMEM rather than panic. */
static int allocation_pressure(void) {
    struct profile_config small = config(PROFILE_FULL,1);
    struct genrt_profile_header before, after;
    CHECK(genrt_profile_control(PROFILE_START,&small) == 0);
    CHECK(genrt_profile_control(PROFILE_STOP,NULL) == 0 && header(&before));
    pid_t children[3];
    uint64_t allocate_at = now_ns() + 1000000000;
    for (size_t cpu = 1; cpu <= 3; cpu++) {
        CHECK(sched_setforkaffinity((int)cpu) == 0);
        children[cpu-1] = fork(); CHECK(children[cpu-1] >= 0);
        if (children[cpu-1] == 0) {
            /* Finish all forks before any CPU reserves a large trace buffer. */
            if (sleep_until(allocate_at) != 0) exit(124);
            struct profile_config c = config(PROFILE_FULL,GENRT_PROFILE_MAX_RECORDS);
            int result = genrt_profile_control(PROFILE_START,&c);
            if (result != 0 && result != -GENRT_ENOMEM) exit(123);
            if (sleep_until(allocate_at + 2000000000) != 0) exit(124);
            exit(result == -GENRT_ENOMEM ? 12 : 0);
        }
    }
    CHECK(sleep_until(allocate_at + 500000000) == 0);
    struct profile_config c = config(PROFILE_FULL,GENRT_PROFILE_MAX_RECORDS);
    int result = genrt_profile_control(PROFILE_START,&c);
    CHECK(result == 0 || result == -GENRT_ENOMEM);
    int failures = result == -GENRT_ENOMEM;
    if (result == -GENRT_ENOMEM) CHECK(header(&after) && memcmp(&before,&after,sizeof(before)) == 0);
    if (result == 0) CHECK(genrt_profile_control(PROFILE_DISCARD,NULL) == 0);
    for (int i = 0; i < 3; i++) {
        int status; CHECK(waitpid(children[i],&status,0) == children[i] && WIFEXITED(status));
        CHECK(WEXITSTATUS(status) == 0 || WEXITSTATUS(status) == 12);
        failures += WEXITSTATUS(status) == 12;
    }
    CHECK(failures > 0);
    c.max_records = 1;
    CHECK(genrt_profile_control(PROFILE_START,&c) == 0);
    CHECK(genrt_profile_control(PROFILE_DISCARD,NULL) == 0);
    return 0;
}
static int fault_owner(void) {
    struct profile_config c = config(PROFILE_FULL,GENRT_PROFILE_MAX_RECORDS);
    CHECK(genrt_profile_control(PROFILE_START,&c) == 0);
    *(volatile int *)0 = 1;
    return 125;
}

static void scenario(const char *name, int cpu, int (*run)(void), int fault) {
    gtrt_case_start(producer,name);
    if (sched_setforkaffinity(cpu) != 0) gtrt_fail(producer,name,"AFFINITY");
    pid_t child = fork();
    if (child == 0) exit(run());
    int status;
    if (child < 0 || waitpid(child,&status,0) != child
        || (fault ? status != 0x7f : !WIFEXITED(status) || WEXITSTATUS(status) != 0)) {
        dprintf(STDERR_FILENO,"profile scenario %s status=%d\n",name,child < 0 ? -1 : status);
        gtrt_fail(producer,name,"ASSERTION");
    }
    gtrt_pass(producer,name);
}

int main(void) {
    gtrt_ready(producer,"wake-profile-contract");
    scenario("profile-validation",0,validation,0);
    scenario("profile-idle-exact",3,idle_trace,0);
    scenario("profile-focused-loss",3,focused_loss,0);
    scenario("profile-generic-rr",3,generic_rr,0);
    scenario("profile-generic-exit",3,generic_exit,0);
    scenario("profile-generic-block",3,generic_block,0);
    scenario("profile-chunks-owner",3,chunks_and_owner,0);
    scenario("profile-fault-cleanup",3,fault_owner,1);
    scenario("profile-post-fault-reuse",3,idle_trace,0);
    /* Run directly in /init: four process slots allow this owner plus three
     * children, not an additional scenario wrapper process. */
    gtrt_case_start(producer,"profile-allocation-pressure");
    if (allocation_pressure() != 0) gtrt_fail(producer,"profile-allocation-pressure","ASSERTION");
    gtrt_pass(producer,"profile-allocation-pressure");
    const char *name="profile-rtbench-report";
    gtrt_case_start(producer,name);
    if (sched_setforkaffinity(3) != 0) gtrt_fail(producer,name,"AFFINITY");
    pid_t child=fork();
    if (child == 0) {
        char *argv[]={"rtbench","-p","5000000","-n","4","-o","30","-P",NULL};
        execve("/bin/rtbench",argv,NULL);exit(126);
    }
    if (!wait_exit(child,0)) gtrt_fail(producer,name,"RTBENCH");
    gtrt_pass(producer,name);
    name="profile-rtbench-focused-report";
    gtrt_case_start(producer,name);
    if (sched_setforkaffinity(3) != 0) gtrt_fail(producer,name,"AFFINITY");
    child=fork();
    if (child == 0) {
        char *argv[]={"rtbench","-p","5000000","-n","4","-o","30","-F","5:8","-v",NULL};
        execve("/bin/rtbench",argv,NULL);exit(126);
    }
    if (!wait_exit(child,0)) gtrt_fail(producer,name,"RTBENCH");
    gtrt_pass(producer,name);
    gtrt_done(producer,"wake-profile-contract");
}
