#include "protocol.h"

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

int main(int argc, char **argv) {
    if (argc >= 2 && string_equal(argv[1], "argv") && argc == 4
        && string_equal(argv[2], "alpha") && string_equal(argv[3], "beta")) {
        gtrt_case_start("shell-argv", "argv");
        gtrt_pass("shell-argv", "argv");
        return 0;
    }
    if (argc >= 2 && string_equal(argv[1], "cwd")) {
        gtrt_case_start("shell-cwd", "cwd");
        char cwd[64];
        if (getcwd(cwd, sizeof(cwd)) != NULL
            && string_equal(cwd, "/.__genrt_test__/fixtures/directory")) {
            gtrt_pass("shell-cwd", "cwd");
            return 0;
        }
        return 2;
    }
    if (argc == 3 && string_equal(argv[1], "nonce")) {
        gtrt_case_start("shell-nonce", "uart-rx");
        gtrt_emit("shell-nonce", "PASS", "uart-rx", argv[2]);
        return 0;
    }
    if (argc == 2 && string_equal(argv[1], "recovered")) {
        gtrt_case_start("shell-recovery", "recovered");
        gtrt_pass("shell-recovery", "recovered");
        return 0;
    }
    if (argc == 2 && string_equal(argv[1], "exit-seven")) {
        gtrt_case_start("shell-exit-seven", "abnormal-child");
        gtrt_pass("shell-exit-seven", "abnormal-child");
        return 7;
    }
    return 64;
}
