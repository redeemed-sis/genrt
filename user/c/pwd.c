#include "syscall.h"
#include <unistd.h>

#include <stdio.h>

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;

    char path[PATH_MAX + 1];
    if (getcwd(path, sizeof(path)) == NULL) {
        write(STDERR_FILENO, "pwd: failed\n", 12);
        return 1;
    }

    if (printf("%s\n", path) < 0) {
        return 1;
    }
    return 0;
}
