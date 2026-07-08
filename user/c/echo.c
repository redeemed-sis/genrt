#include "syscall.h"

static size_t strlen_local(const char *s) {
    size_t len = 0;
    while (s[len] != '\0') {
        len++;
    }
    return len;
}

int main(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        if (i > 1) {
            write(1, " ", 1);
        }
        write(1, argv[i], strlen_local(argv[i]));
    }
    write(1, "\n", 1);
    return 0;
}
