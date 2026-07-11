#include "syscall.h"

static void write_lit(const char *s, size_t len) {
    write(1, s, len);
}

static int copy_fd_to_stdout(int fd) {
    char buf[128];

    for (;;) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n < 0) {
            write_lit("cat: read failed\n", 17);
            return 1;
        }
        if (n == 0) {
            return 0;
        }
        if (write(1, buf, (size_t)n) < 0) {
            return 1;
        }
    }
}

int main(int argc, char **argv) {
    if (argc < 2) {
        return copy_fd_to_stdout(0);
    }

    int failed = 0;
    for (int i = 1; i < argc; i++) {
        int fd = open(argv[i], O_RDONLY);
        if (fd < 0) {
            write_lit("cat: cannot open\n", 17);
            failed = 1;
            continue;
        }

        if (copy_fd_to_stdout(fd) != 0) {
            failed = 1;
        }
        close(fd);
    }

    return failed;
}
