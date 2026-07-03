#include "syscall.h"

#define SHELL_LINE_MAX 256

static size_t strlen_lit(const char *s) {
    size_t len = 0;
    while (s[len] != '\0') {
        len++;
    }
    return len;
}

static void puts_lit(const char *s) {
    write(1, s, strlen_lit(s));
}

static int streq(const char *a, const char *b) {
    size_t i = 0;
    while (a[i] != '\0' && b[i] != '\0') {
        if (a[i] != b[i]) {
            return 0;
        }
        i++;
    }
    return a[i] == b[i];
}

static void print_file(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        puts_lit("open failed\n");
        return;
    }

    char buf[64];
    for (;;) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n < 0) {
            puts_lit("read failed\n");
            close(fd);
            return;
        }
        if (n == 0) {
            break;
        }
        write(1, buf, (size_t)n);
    }

    close(fd);
}

int main(void) {
    char line[SHELL_LINE_MAX + 1];
    int skip_lf = 0;

    puts_lit("genrt shell\n");

    for (;;) {
        size_t len = 0;
        int overflow = 0;
        puts_lit("> ");

        for (;;) {
            char ch;
            ssize_t n = read(0, &ch, 1);
            if (n < 0) {
                puts_lit("read failed\n");
                return 1;
            }
            if (n == 0) {
                continue;
            }

            if (skip_lf && ch == '\n') {
                skip_lf = 0;
                continue;
            }
            skip_lf = 0;

            if (ch == '\r' || ch == '\n') {
                if (ch == '\r') {
                    skip_lf = 1;
                }
                puts_lit("\n");
                break;
            }

            if (ch == '\b' || ch == 0x7f) {
                if (len != 0) {
                    len--;
                    puts_lit("\b \b");
                }
                continue;
            }

            if (len < SHELL_LINE_MAX) {
                line[len++] = ch;
                write(1, &ch, 1);
            } else {
                overflow = 1;
            }
        }

        line[len] = '\0';
        if (overflow) {
            puts_lit("line too long\n");
            continue;
        }
        if (len == 0) {
            continue;
        }
        if (streq(line, "exit")) {
            return 0;
        }

        print_file(line);
    }
}
