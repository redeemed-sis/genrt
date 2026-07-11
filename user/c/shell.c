#include "syscall.h"

#define SHELL_LINE_MAX 256
#define SHELL_ARG_MAX 16
#define SHELL_PATH_MAX 128

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

static int contains_char(const char *s, char needle) {
    for (size_t i = 0; s[i] != '\0'; i++) {
        if (s[i] == needle) {
            return 1;
        }
    }
    return 0;
}

static int starts_with(const char *s, const char *prefix) {
    size_t i = 0;
    while (prefix[i] != '\0') {
        if (s[i] != prefix[i]) {
            return 0;
        }
        i++;
    }
    return 1;
}

static void copy_str(char *dst, const char *src, size_t dst_len) {
    if (dst_len == 0) {
        return;
    }

    size_t i = 0;
    while (i + 1 < dst_len && src[i] != '\0') {
        dst[i] = src[i];
        i++;
    }
    dst[i] = '\0';
}

static int make_bin_path(char *dst, size_t dst_len, const char *cmd) {
    const char prefix[] = "/bin/";
    size_t prefix_len = sizeof(prefix) - 1;
    size_t cmd_len = strlen_lit(cmd);

    if (prefix_len + cmd_len + 1 > dst_len) {
        return -1;
    }

    for (size_t i = 0; i < prefix_len; i++) {
        dst[i] = prefix[i];
    }
    for (size_t i = 0; i < cmd_len; i++) {
        dst[prefix_len + i] = cmd[i];
    }
    dst[prefix_len + cmd_len] = '\0';
    return 0;
}

static int split_words(char *line, char **argv, int max_args) {
    int argc = 0;
    char *p = line;

    while (*p != '\0') {
        while (*p == ' ' || *p == '\t') {
            p++;
        }
        if (*p == '\0') {
            break;
        }
        if (argc == max_args) {
            return -1;
        }

        argv[argc++] = p;
        while (*p != '\0' && *p != ' ' && *p != '\t') {
            p++;
        }
        if (*p != '\0') {
            *p++ = '\0';
        }
    }

    argv[argc] = NULL;
    return argc;
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

static void run_command(char *line) {
    char *argv[SHELL_ARG_MAX + 1];
    char path[SHELL_PATH_MAX];
    int argc = split_words(line, argv, SHELL_ARG_MAX);

    if (argc < 0) {
        puts_lit("too many args\n");
        return;
    }
    if (argc == 0) {
        return;
    }
    if (streq(argv[0], "exit")) {
        exit(0);
    }
    if (streq(argv[0], "cd")) {
        if (argc > 2) {
            puts_lit("cd: usage\n");
            return;
        }
        const char *target = argc == 1 ? "/" : argv[1];
        if (chdir(target) < 0) {
            puts_lit("cd: failed\n");
        }
        return;
    }

    if (argv[0][0] == '/' && argc == 1 && !starts_with(argv[0], "/bin/")) {
        print_file(argv[0]);
        return;
    }

    if (contains_char(argv[0], '/')) {
        copy_str(path, argv[0], sizeof(path));
    } else if (make_bin_path(path, sizeof(path), argv[0]) != 0) {
        puts_lit("command path too long\n");
        return;
    }

    pid_t pid = fork();
    if (pid < 0) {
        puts_lit("fork failed\n");
        return;
    }
    if (pid == 0) {
        execve(path, argv, NULL);
        puts_lit("exec failed\n");
        exit(127);
    }

    int status = 0;
    pid_t waited = waitpid(pid, &status, 0);
    if (waited < 0) {
        puts_lit("wait failed\n");
        return;
    }
    if (!WIFEXITED(status)) {
        puts_lit("command faulted\n");
    }
}

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;

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
        run_command(line);
    }
}
