#include "affinity.h"
#include "protocol.h"

#define CHILD_EXIT_CODE 41
#define MAX_EXHAUSTION_CHILDREN 31

static const char *producer = "smp-api-supervisor";

static int wait_for_exit(pid_t child, int expected) {
    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status)
           && WEXITSTATUS(status) == expected;
}

static pid_t fork_exit_child(size_t expected_cpu) {
    pid_t child = fork();
    if (child == 0) {
        exit(gtrt_affinity_is_only(0, expected_cpu) ? CHILD_EXIT_CODE : 70);
    }
    return child;
}

static int string_equal(const char *lhs, const char *rhs) {
    size_t index = 0;
    while (lhs[index] != '\0' && rhs[index] != '\0') {
        if (lhs[index] != rhs[index]) {
            return 0;
        }
        index++;
    }
    return lhs[index] == rhs[index];
}

static void explicit_placement(void) {
    const char *name = "explicit-placement";
    gtrt_case_start(producer, name);
    if (sched_setforkaffinity(1) != 0) {
        gtrt_fail(producer, name, "SET");
    }
    pid_t child = fork_exit_child(1);
    if (child < 0 || !gtrt_affinity_is_only(child, 1)
        || !wait_for_exit(child, CHILD_EXIT_CODE)) {
        gtrt_fail(producer, name, "CHILD");
    }
    gtrt_pass(producer, name);
}

static void one_shot_placement(void) {
    const char *name = "one-shot-placement";
    gtrt_case_start(producer, name);
    if (sched_setforkaffinity(1) != 0) {
        gtrt_fail(producer, name, "SET");
    }
    pid_t explicit_child = fork_exit_child(1);
    if (explicit_child < 0
        || !wait_for_exit(explicit_child, CHILD_EXIT_CODE)) {
        gtrt_fail(producer, name, "EXPLICIT_CHILD");
    }
    pid_t default_child = fork_exit_child(0);
    if (default_child < 0 || !wait_for_exit(default_child, CHILD_EXIT_CODE)) {
        gtrt_fail(producer, name, "DEFAULT_CHILD");
    }
    gtrt_pass(producer, name);
}

static void override_placement(void) {
    const char *name = "override-placement";
    gtrt_case_start(producer, name);
    if (sched_setforkaffinity(1) != 0 || sched_setforkaffinity(2) != 0) {
        gtrt_fail(producer, name, "SET");
    }
    pid_t child = fork_exit_child(2);
    if (child < 0 || !wait_for_exit(child, CHILD_EXIT_CODE)) {
        gtrt_fail(producer, name, "CHILD");
    }
    gtrt_pass(producer, name);
}

static void reset_placement(void) {
    const char *name = "reset-placement";
    gtrt_case_start(producer, name);
    if (sched_setforkaffinity(1) != 0 || sched_setforkaffinity(-1) != 0) {
        gtrt_fail(producer, name, "RESET");
    }
    pid_t child = fork_exit_child(0);
    if (child < 0 || !wait_for_exit(child, CHILD_EXIT_CODE)) {
        gtrt_fail(producer, name, "CHILD");
    }
    gtrt_pass(producer, name);
}

static void failed_fork_preserves_placement(void) {
    const char *name = "failed-fork-preserves-placement";
    pid_t children[MAX_EXHAUSTION_CHILDREN];
    size_t child_count = 0;

    gtrt_case_start(producer, name);
    while (child_count < MAX_EXHAUSTION_CHILDREN) {
        pid_t child = fork_exit_child(0);
        if (child < 0) {
            break;
        }
        children[child_count++] = child;
    }
    if (child_count == 0 || child_count == MAX_EXHAUSTION_CHILDREN) {
        gtrt_fail(producer, name, "NO_BOUNDED_EXHAUSTION");
    }
    if (sched_setforkaffinity(3) != 0 || fork_exit_child(3) >= 0) {
        gtrt_fail(producer, name, "EXPECTED_FAILURE");
    }
    if (!wait_for_exit(children[0], CHILD_EXIT_CODE)) {
        gtrt_fail(producer, name, "REAP_FOR_RETRY");
    }
    pid_t retry = fork_exit_child(3);
    if (retry < 0 || !wait_for_exit(retry, CHILD_EXIT_CODE)) {
        gtrt_fail(producer, name, "RETRY");
    }
    for (size_t index = 1; index < child_count; index++) {
        if (!wait_for_exit(children[index], CHILD_EXIT_CODE)) {
            gtrt_fail(producer, name, "REAP_REMAINDER");
        }
    }
    gtrt_pass(producer, name);
}

static void remote_exec(void) {
    const char *name = "remote-exec";
    gtrt_case_start(producer, name);
    if (sched_setforkaffinity(2) != 0) {
        gtrt_fail(producer, name, "SET");
    }
    pid_t child = fork();
    if (child < 0) {
        gtrt_fail(producer, name, "FORK");
    }
    if (child == 0) {
        char *argv[] = {"echo", "smp-contract", NULL};
        execve("/bin/echo", argv, NULL);
        exit(126);
    }
    if (!wait_for_exit(child, 0)) {
        gtrt_fail(producer, name, "CHILD_STATUS");
    }
    gtrt_pass(producer, name);
}

static void exec_preserves_affinity(void) {
    const char *name = "exec-preserves-affinity";
    gtrt_case_start(producer, name);
    if (sched_setforkaffinity(2) != 0) {
        gtrt_fail(producer, name, "SET");
    }
    pid_t child = fork();
    if (child < 0) {
        gtrt_fail(producer, name, "FORK");
    }
    if (child == 0) {
        char *argv[] = {"init", "--affinity-probe", "2", NULL};
        execve("/init", argv, NULL);
        exit(126);
    }
    if (!gtrt_affinity_is_only(child, 2)
        || !wait_for_exit(child, CHILD_EXIT_CODE)) {
        gtrt_fail(producer, name, "CHILD");
    }
    gtrt_pass(producer, name);
}

static void remote_fault_reap(void) {
    const char *name = "remote-fault-reap";
    gtrt_case_start(producer, name);
    if (sched_setforkaffinity(3) != 0) {
        gtrt_fail(producer, name, "SET");
    }
    pid_t child = fork();
    if (child < 0) {
        gtrt_fail(producer, name, "FORK");
    }
    if (child == 0) {
        char *argv[] = {"fault-null", NULL};
        execve("/.__genrt_test__/bin/fault-null", argv, NULL);
        exit(126);
    }
    if (!gtrt_affinity_is_only(child, 3)) {
        gtrt_fail(producer, name, "AFFINITY");
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child || status != 0x7f) {
        gtrt_fail(producer, name, "FAULT_STATUS");
    }
    gtrt_pass(producer, name);
}

static void affinity_validation(void) {
    const char *name = "affinity-validation";
    cpu_set_t mask;
    gtrt_case_start(producer, name);
    if (sched_setforkaffinity(4) >= 0 || sched_setforkaffinity(-2) >= 0
        || sched_setforkaffinity(-1) != 0 || !gtrt_affinity_is_only(0, 0)
        || sched_getaffinity(-1, sizeof(mask), &mask) != -GTRT_ESRCH) {
        gtrt_fail(producer, name, "INVALID_ACCEPTED");
    }
    gtrt_pass(producer, name);
}

int main(int argc, char **argv) {
    if (argc == 3 && string_equal(argv[1], "--affinity-probe")) {
        if (argv[2][0] < '0' || argv[2][0] > '3' || argv[2][1] != '\0') {
            return 71;
        }
        return gtrt_affinity_is_only(0, (size_t)(argv[2][0] - '0'))
                   ? CHILD_EXIT_CODE
                   : 72;
    }

    gtrt_ready(producer, "smp-userspace-contract");
    explicit_placement();
    one_shot_placement();
    override_placement();
    reset_placement();
    failed_fork_preserves_placement();
    remote_exec();
    exec_preserves_affinity();
    remote_fault_reap();
    affinity_validation();
    gtrt_done(producer, "smp-userspace-contract");
}
