#include "protocol.h"

#include <string.h>

// shell-contract.toml supplies UART input to the production shell, which
// fork/execs this probe as p or a /bin/gtrt-* completion alias. These checks
// observe the resulting argv/cwd, not the echoed input or terminal rendering.
int main(int argc, char **argv) {
    // History setup: p record VALUE -> argc=3, argv=[p, record, VALUE].
    // Silent seeds can be recalled repeatedly without restarting a protocol
    // producer. Each asserting invocation carries its own unique case ID.
    if (argc == 3 && strcmp(argv[1], "record") == 0) return 0;
    // After recall/edit: p record ACTUAL CASE_ID EXPECTED -> argc=5.
    // For example, Up + " history-edit alpha" recalls "p record alpha".
    // argv[2] is the preserved value; argv[3:5] are appended test arguments.
    if (argc == 5 && strcmp(argv[1], "record") == 0) {
        gtrt_case_start(argv[3], argv[3]);
        if (strcmp(argv[2], argv[4]) != 0) gtrt_fail(argv[3], argv[3], "HISTORY_VALUE");
        gtrt_pass(argv[3], argv[3]);
        return 0;
    }
    // After Tab: COMMAND completion CASE_ID EXPECTED_COMMAND -> argc=4.
    // argv[0] must be the completed token, including /bin/ for path input.
    if (argc == 4 && strcmp(argv[1], "completion") == 0) {
        gtrt_case_start(argv[2], argv[2]);
        if (strcmp(argv[0], argv[3]) != 0) gtrt_fail(argv[2], argv[2], "COMPLETED_ARGV");
        gtrt_pass(argv[2], argv[2]);
        return 0;
    }
    // Negative inputs: p rejected <overlong word or too many arguments>.
    // The shell must reject both before exec; reaching this branch is failure.
    if (argc >= 2 && strcmp(argv[1], "rejected") == 0) {
        gtrt_fail("shell-limit", "shell-limit", "INVALID_LINE_EXECUTED");
    }
    // After "cd<Tab>/.__genrt_test__/fixtures/directory", the host submits
    // "../../bin/p directory tab-builtin" (argc=3). Check inherited cwd.
    if (argc == 3 && strcmp(argv[1], "directory") == 0) {
        char cwd[64];
        gtrt_case_start(argv[2], argv[2]);
        if (getcwd(cwd, sizeof(cwd)) == NULL
            || strcmp(cwd, "/.__genrt_test__/fixtures/directory") != 0) {
            gtrt_fail(argv[2], argv[2], "BUILTIN_COMPLETION");
        }
        gtrt_pass(argv[2], argv[2]);
        return 0;
    }
    // Initial parsing check: ./p argv alpha beta -> argc=4.
    if (argc >= 2 && (strcmp(argv[1], "argv") == 0) && argc == 4
        && (strcmp(argv[2], "alpha") == 0) && (strcmp(argv[3], "beta") == 0)) {
        gtrt_case_start("shell-argv", "argv");
        gtrt_pass("shell-argv", "argv");
        return 0;
    }
    // After relative cd: ../../bin/p cwd -> argc=2; exec must preserve cwd.
    if (argc >= 2 && (strcmp(argv[1], "cwd") == 0)) {
        gtrt_case_start("shell-cwd", "cwd");
        char cwd[64];
        if (getcwd(cwd, sizeof(cwd)) != NULL
            && (strcmp(cwd, "/.__genrt_test__/fixtures/directory") == 0)) {
            gtrt_pass("shell-cwd", "cwd");
            return 0;
        }
        return 2;
    }
    // UART challenge: ../../bin/p nonce HEX_TOKEN -> argc=3.
    // The runner generates argv[2] and requires that exact protocol detail.
    if (argc == 3 && (strcmp(argv[1], "nonce") == 0)) {
        gtrt_case_start("shell-nonce", "uart-rx");
        gtrt_emit("shell-nonce", "PASS", "uart-rx", argv[2]);
        return 0;
    }
    // After "p exit-seven" and failed exec of "x": p recovered -> argc=2.
    // Reaching this invocation proves the shell still accepts commands.
    if (argc == 2 && (strcmp(argv[1], "recovered") == 0)) {
        gtrt_case_start("shell-recovery", "recovered");
        gtrt_pass("shell-recovery", "recovered");
        return 0;
    }
    // p exit-seven -> argc=2: report arrival, then deliberately exit nonzero.
    // Continuation is checked by the later recovered invocation, not this PASS.
    if (argc == 2 && (strcmp(argv[1], "exit-seven") == 0)) {
        gtrt_case_start("shell-exit-seven", "abnormal-child");
        gtrt_pass("shell-exit-seven", "abnormal-child");
        return 7;
    }
    return 64;
}
