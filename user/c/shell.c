#include "syscall.h"
#include <unistd.h>
#include <stdio.h>
#include <string.h>

#define SHELL_LINE_MAX 256
#define SHELL_ARG_MAX 16
#define SHELL_PATH_MAX 128

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
        printf("%s", "open failed\n");
        return;
    }

    char buf[64];
    for (;;) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n < 0) {
            printf("%s", "read failed\n");
            close(fd);
            return;
        }
        if (n == 0) {
            break;
        }
        write(STDOUT_FILENO, buf, (size_t)n);
    }

    close(fd);
}

static void run_command(char *line) {
    char *argv[SHELL_ARG_MAX + 1];
    char path[SHELL_PATH_MAX];
    int argc = split_words(line, argv, SHELL_ARG_MAX);

    if (argc < 0) {
        printf("%s", "too many args\n");
        return;
    }
    if (argc == 0) {
        return;
    }
    if (strcmp(argv[0], "exit") == 0) {
        exit(0);
    }
    if (strcmp(argv[0], "cd") == 0) {
        if (argc > 2) {
            printf("%s", "cd: usage\n");
            return;
        }
        const char *target = argc == 1 ? "/" : argv[1];
        if (chdir(target) < 0) {
            printf("%s", "cd: failed\n");
        }
        return;
    }

    if (argv[0][0] == '/' && argc == 1 && strncmp(argv[0], "/bin/", 5) != 0) {
        print_file(argv[0]);
        return;
    }

    if (strchr(argv[0], '/') != NULL) {
        snprintf(path, sizeof(path), "%s", argv[0]);
    } else if (snprintf(path, sizeof(path), "/bin/%s", argv[0]) >= (int)sizeof(path)) {
        printf("%s", "command path too long\n");
        return;
    }

    pid_t pid = fork();
    if (pid < 0) {
        printf("%s", "fork failed\n");
        return;
    }
    if (pid == 0) {
        execve(path, argv, NULL);
        printf("%s", "exec failed\n");
        exit(127);
    }

    int status = 0;
    pid_t waited = waitpid(pid, &status, 0);
    if (waited < 0) {
        printf("%s", "wait failed\n");
        return;
    }
    if (!WIFEXITED(status)) {
        printf("%s", "command faulted\n");
    }
}

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;

    char line[SHELL_LINE_MAX + 1];
    int skip_lf = 0;

    printf("%s", "genrt shell\n");

    for (;;) {
        size_t len = 0;
        int overflow = 0;
        printf("%s", "> ");

        for (;;) {
            char ch;
            ssize_t n = read(STDIN_FILENO, &ch, 1);
            if (n < 0) {
                printf("%s", "read failed\n");
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
                printf("%s", "\n");
                break;
            }

            if (ch == '\b' || ch == 0x7f) {
                if (len != 0) {
                    len--;
                    printf("%s", "\b \b");
                }
                continue;
            }

            if (len < SHELL_LINE_MAX) {
                line[len++] = ch;
                write(STDOUT_FILENO, &ch, 1);
            } else {
                overflow = 1;
            }
        }

        line[len] = '\0';
        if (overflow) {
            printf("%s", "line too long\n");
            continue;
        }
        if (len == 0) {
            continue;
        }
        run_command(line);
    }
}
