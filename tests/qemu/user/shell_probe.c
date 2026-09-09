#include "protocol.h"

#include <string.h>

int main(int argc, char **argv) {
    if (argc >= 2 && (strcmp(argv[1], "argv") == 0) && argc == 4
        && (strcmp(argv[2], "alpha") == 0) && (strcmp(argv[3], "beta") == 0)) {
        gtrt_case_start("shell-argv", "argv");
        gtrt_pass("shell-argv", "argv");
        return 0;
    }
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
    if (argc == 3 && (strcmp(argv[1], "nonce") == 0)) {
        gtrt_case_start("shell-nonce", "uart-rx");
        gtrt_emit("shell-nonce", "PASS", "uart-rx", argv[2]);
        return 0;
    }
    if (argc == 2 && (strcmp(argv[1], "recovered") == 0)) {
        gtrt_case_start("shell-recovery", "recovered");
        gtrt_pass("shell-recovery", "recovered");
        return 0;
    }
    if (argc == 2 && (strcmp(argv[1], "exit-seven") == 0)) {
        gtrt_case_start("shell-exit-seven", "abnormal-child");
        gtrt_pass("shell-exit-seven", "abnormal-child");
        return 7;
    }
    return 64;
}
