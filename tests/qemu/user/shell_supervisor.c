#include "protocol.h"
#include "product_contracts.h"

static const char *producer = "shell-supervisor";

int main(void) {
    const struct gtrt_program_contract *contract = &GTRT_PROGRAM_CONTRACTS[0];
    gtrt_ready(producer, "shell-contract");
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
    gtrt_done(producer, "shell-contract");
}
