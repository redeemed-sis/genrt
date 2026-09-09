#include "syscall.h"
#include <unistd.h>
#include <stdio.h>
#include <string.h>

#define LS_BUF_SIZE 512
#define DIRENT64_HEADER_SIZE 19

static int list_path(const char *path) {
    int fd = open(path, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        printf("ls: cannot open\n");
        return 1;
    }

    char buf[LS_BUF_SIZE] __attribute__((aligned(8)));
    for (;;) {
        long n = getdents64(fd, buf, sizeof(buf));
        if (n < 0) {
            printf("ls: cannot read\n");
            close(fd);
            return 1;
        }
        if (n == 0) {
            close(fd);
            return 0;
        }

        size_t offset = 0;
        while (offset < (size_t)n) {
            struct genrt_dirent64 *entry = (struct genrt_dirent64 *)(buf + offset);
            if (entry->d_reclen < DIRENT64_HEADER_SIZE ||
                offset + entry->d_reclen > (size_t)n) {
                printf("ls: bad dirent\n");
                close(fd);
                return 1;
            }

            size_t name_len = strnlen(entry->d_name,
                                      entry->d_reclen - DIRENT64_HEADER_SIZE);
            write(STDOUT_FILENO, entry->d_name, name_len);
            printf("\n");
            offset += entry->d_reclen;
        }
    }
}

int main(int argc, char **argv) {
    if (argc < 2) {
        return list_path(".");
    }

    int failed = 0;
    for (int i = 1; i < argc; i++) {
        if (list_path(argv[i]) != 0) {
            failed = 1;
        }
    }
    return failed;
}
