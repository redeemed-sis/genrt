#include "protocol.h"
#include "product_contracts.h"

#if defined(GTRT_POWER_REBOOT) == defined(GTRT_POWER_POWEROFF)
#error "select exactly one power operation"
#endif

#if defined(GTRT_POWER_REBOOT)
#define GTRT_POWER_PRODUCER "reboot-supervisor"
#define GTRT_POWER_SUITE "reboot-contract"
#else
#define GTRT_POWER_PRODUCER "poweroff-supervisor"
#define GTRT_POWER_SUITE "poweroff-contract"
#endif

_Static_assert(GTRT_PROGRAM_CONTRACT_COUNT == 1,
               "power supervisor requires one product invocation");

static int await_host_trigger(void) {
    char input[3];
    size_t used = 0;
    while (used < sizeof(input)) {
        ssize_t count = read(0, &input[used], 1);
        if (count != 1) {
            return 0;
        }
        if (input[used++] == '\n') {
            break;
        }
    }
    return used == 3 && input[0] == 'g' && input[1] == 'o'
           && input[2] == '\n';
}

int main(void) {
    const struct gtrt_program_contract *contract =
        &GTRT_PROGRAM_CONTRACTS[0];

    gtrt_ready(GTRT_POWER_PRODUCER, GTRT_POWER_SUITE);
    if (!await_host_trigger()) {
        gtrt_case_start(GTRT_POWER_PRODUCER, contract->case_name);
        gtrt_fail(GTRT_POWER_PRODUCER, contract->case_name, "TRIGGER");
    }
    gtrt_case_start(GTRT_POWER_PRODUCER, contract->case_name);

#if defined(GTRT_POWER_REBOOT)
    gtrt_terminal(GTRT_POWER_PRODUCER, contract->case_name, "RESTART");
#else
    gtrt_terminal(GTRT_POWER_PRODUCER, contract->case_name, "POWER_OFF");
#endif

#if defined(GTRT_POWER_REMOTE)
    char *argv[] = {"taskset", "1", (char *)contract->path, NULL};
    execve("/bin/taskset", argv, NULL);
#else
    execve(contract->path, contract->argv, NULL);
#endif

    gtrt_fail(GTRT_POWER_PRODUCER, contract->case_name, "EXEC");
}
