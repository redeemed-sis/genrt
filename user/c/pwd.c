#include "syscall.h"

static size_t string_length(const char *value) {
    size_t length = 0;
    while (value[length] != '\0') {
        length++;
    }
    return length;
}

static int write_all(int fd, const char *buffer, size_t length) {
    size_t written = 0;
    while (written < length) {
        ssize_t result = write(fd, buffer + written, length - written);
        if (result <= 0) {
            return -1;
        }
        written += (size_t)result;
    }
    return 0;
}

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;

    char path[PATH_MAX + 1];
    if (getcwd(path, sizeof(path)) == NULL) {
        write(2, "pwd: failed\n", 12);
        return 1;
    }

    if (write_all(1, path, string_length(path)) != 0 || write_all(1, "\n", 1) != 0) {
        return 1;
    }
    return 0;
}
