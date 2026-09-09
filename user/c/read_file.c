#include "syscall.h"
#include <unistd.h>
#include <stdio.h>

int main(void) {
    int fd = open("/hello.txt", O_RDONLY);
    if (fd < 0) {
        printf("open failed\n");
        return 1;
    }

    char buf[16];
    for (;;) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n < 0) {
            printf("read failed\n");
            close(fd);
            return 2;
        }
        if (n == 0) {
            break;
        }
        write(STDOUT_FILENO, buf, (size_t)n);
    }

    close(fd);
    return 0;
}
