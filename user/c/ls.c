#include "syscall.h"

#define LS_BUF_SIZE 512
#define DIRENT64_HEADER_SIZE 19

static void write_lit(const char *s, size_t len) {
    write(1, s, len);
}

static int list_path(const char *path) {
    int fd = open(path, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        write_lit("ls: cannot open\n", 16);
        return 1;
    }

    char buf[LS_BUF_SIZE] __attribute__((aligned(8)));
    for (;;) {
        long n = getdents64(fd, buf, sizeof(buf));
        if (n < 0) {
            write_lit("ls: cannot read\n", 16);
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
                write_lit("ls: bad dirent\n", 15);
                close(fd);
                return 1;
            }

            size_t name_len = 0;
            while (name_len + DIRENT64_HEADER_SIZE < entry->d_reclen &&
                   entry->d_name[name_len] != '\0') {
                name_len++;
            }
            write(1, entry->d_name, name_len);
            write_lit("\n", 1);
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
