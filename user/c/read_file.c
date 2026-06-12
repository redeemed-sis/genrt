#include "syscall.h"

int main(void) {
    int fd = open("/hello.txt", O_RDONLY);
    if (fd < 0) {
        write(1, "open failed\n", 12);
        return 1;
    }

    char buf[16];
    for (;;) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n < 0) {
            write(1, "read failed\n", 12);
            close(fd);
            return 2;
        }
        if (n == 0) {
            break;
        }
        write(1, buf, (size_t)n);
    }

    close(fd);
    return 0;
}
