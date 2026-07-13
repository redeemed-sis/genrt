#include "protocol.h"
#include "product_contracts.h"

static const char *producer = "api-supervisor";

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
    pid_t child = fork();
    if (child < 0) {
        gtrt_fail(producer, contract->case_name, "FORK");
    }
    if (child == 0) {
        execve(contract->path, contract->argv, NULL);
        exit(126);
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)
        || WEXITSTATUS(status) != contract->expected_exit) {
        gtrt_fail(producer, contract->case_name, "CHILD_STATUS");
    }
    gtrt_pass(producer, contract->case_name);
}

int main(void) {
    gtrt_ready(producer, "userspace-contract");
    run_case("file-io");
    run_case("directory-io");
    run_case("cwd-paths");
    run_case("process-control");
    for (size_t index = 0; index < GTRT_PROGRAM_CONTRACT_COUNT; index++) {
        run_program(&GTRT_PROGRAM_CONTRACTS[index]);
    }
    gtrt_done(producer, "userspace-contract");
}
