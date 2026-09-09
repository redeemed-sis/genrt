#include "syscall.h"
#include <unistd.h>

#include <stdio.h>

int main(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        if (i > 1) {
            write(STDOUT_FILENO, " ", 1);
        }
        printf("%s", argv[i]);
    }
    write(STDOUT_FILENO, "\n", 1);
    return 0;
}
