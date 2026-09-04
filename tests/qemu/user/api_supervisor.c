#include "affinity.h"
#include "protocol.h"
#include "product_contracts.h"

#define TASKSET_LARGE_ARG_BYTES 62000

static const char *producer = "api-supervisor";
static char taskset_large_arg[TASKSET_LARGE_ARG_BYTES + 1];

static int exec_and_wait(const char *path, char *const argv[], int expected) {
    pid_t child = fork();
    if (child < 0) {
        return 0;
    }
    if (child == 0) {
        execve(path, argv, NULL);
        exit(126);
    }

    int status = 0;
    return waitpid(child, &status, 0) == child && WIFEXITED(status)
           && WEXITSTATUS(status) == expected;
}

static void init_affinity(void) {
    const char *name = "init-affinity";
    gtrt_case_start(producer, name);
    if (!gtrt_affinity_is_only(0, 0)) {
        gtrt_fail(producer, name, "MASK");
    }
    gtrt_pass(producer, name);
}

static void run_case(const char *name) {
    gtrt_case_start(producer, name);
    pid_t child = fork();
    if (child < 0) {
        gtrt_fail(producer, name, "FORK");
    }
    if (child == 0) {
        char *argv[] = {"api-case", (char *)name, NULL};
        execve("/.__genrt_test__/bin/api-case", argv, NULL);
        exit(126);
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)
        || WEXITSTATUS(status) != 0) {
        gtrt_fail(producer, name, "CHILD_STATUS");
    }
    gtrt_pass(producer, name);
}

static void run_program(const struct gtrt_program_contract *contract) {
    gtrt_case_start(producer, contract->case_name);
    if (!exec_and_wait(contract->path,
                       contract->argv,
                       contract->expected_exit)) {
        gtrt_fail(producer, contract->case_name, "CHILD_STATUS");
    }
    gtrt_pass(producer, contract->case_name);
}

static void taskset_cli_validation(void) {
    const char *name = "taskset-cli-validation";
    char *valid[] = {"taskset", "0", "/.__genrt_test__/bin/api-case",
                     "taskset-probe", "alpha", "beta", NULL};
    char *missing_cpu[] = {"taskset", NULL};
    char *malformed_cpu[] = {"taskset", "1x", "echo", NULL};
    char *overflow_cpu[] = {"taskset", "2147483648", "echo", NULL};
    char *missing_program[] = {"taskset", "0", NULL};
    char *empty_program[] = {"taskset", "0", "", NULL};
    char *unavailable_cpu[] = {"taskset", "1", "echo", NULL};
    char *missing_executable[] = {"taskset", "0", "missing-executable",
                                  NULL};
    char *child_status[] = {"taskset", "0",
                            "/.__genrt_test__/bin/api-case", "unknown",
                            NULL};
    char *large_argv[] = {"taskset", "0", "/.__genrt_test__/bin/api-case",
                          "taskset-large-probe", taskset_large_arg, NULL};

    gtrt_case_start(producer, name);
    if (!exec_and_wait("/bin/taskset", valid, 0)
        || !exec_and_wait("/bin/taskset", missing_cpu, 2)
        || !exec_and_wait("/bin/taskset", malformed_cpu, 2)
        || !exec_and_wait("/bin/taskset", overflow_cpu, 2)
        || !exec_and_wait("/bin/taskset", missing_program, 2)
        || !exec_and_wait("/bin/taskset", empty_program, 2)
        || !exec_and_wait("/bin/taskset", unavailable_cpu, 1)
        || !exec_and_wait("/bin/taskset", missing_executable, 127)
        || !exec_and_wait("/bin/taskset", child_status, 65)) {
        gtrt_fail(producer, name, "RESULT");
    }

    for (size_t index = 0; index < TASKSET_LARGE_ARG_BYTES; index++) {
        taskset_large_arg[index] = 'x';
    }
    taskset_large_arg[TASKSET_LARGE_ARG_BYTES] = '\0';
    if (!exec_and_wait("/bin/taskset", large_argv, 0)) {
        gtrt_fail(producer, name, "LARGE_ARGV");
    }

    char *lifecycle[] = {"taskset", "0", "echo", NULL};
    for (size_t iteration = 0; iteration < 20; iteration++) {
        if (!exec_and_wait("/bin/taskset", lifecycle, 0)) {
            gtrt_fail(producer, name, "REAP");
        }
    }
    gtrt_pass(producer, name);
}

int main(void) {
    gtrt_ready(producer, "userspace-contract");
    init_affinity();
    run_case("file-io");
    run_case("directory-io");
    run_case("cwd-paths");
    run_case("process-control");
    run_case("fork-affinity-validation");
    run_case("process-affinity-validation");
    taskset_cli_validation();
    for (size_t index = 0; index < GTRT_PROGRAM_CONTRACT_COUNT; index++) {
        run_program(&GTRT_PROGRAM_CONTRACTS[index]);
    }
    gtrt_done(producer, "userspace-contract");
}
