#include "syscall.h"
#include <unistd.h>
#include <stdio.h>
#include <string.h>
#include "shell_input.h"

#define SHELL_ARG_MAX 16
#define SHELL_PATH_MAX 128

static const char *const builtins[] = {"cd", "exit"};
static struct shell_input input;
/* A maximum-length pathname also bounds an individual directory record.
 * Keep the scan buffer off the small userspace runtime stack. */
static char directory_entries[PATH_MAX + 32] __attribute__((aligned(8)));

static int is_builtin(const char *name) {
    for (size_t i = 0; i < sizeof(builtins) / sizeof(builtins[0]); i++) {
        if (strcmp(name, builtins[i]) == 0) return 1;
    }
    return 0;
}

/* getdents64 supplies kind but not mode. This readonly runtime executes
 * static ELF images, so omit directories and non-ET_EXEC files. Inspect only
 * ELF identification/type, not architecture-specific registers or machine IDs;
 * execve remains responsible for validating the complete image. */
static int is_command_image(const char *name) {
    enum { ELF_IDENT_SIZE = 16, ELF_TYPE_BYTES = 2, ELF_DATA = 5,
           ELF_LITTLE_ENDIAN = 1, ELF_BIG_ENDIAN = 2, ELF_EXEC = 2 };
    unsigned char header[ELF_IDENT_SIZE + ELF_TYPE_BYTES];
    char path[SHELL_PATH_MAX];
    if (snprintf(path, sizeof(path), "/bin/%s", name) >= (int)sizeof(path)) return 0;
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    size_t used = 0;
    while (used < sizeof(header)) {
        ssize_t count = read(fd, header + used, sizeof(header) - used);
        if (count <= 0) break;
        used += (size_t)count;
    }
    close(fd);
    if (used != sizeof(header) || memcmp(header, "\177ELF", 4) != 0) return 0;
    if (header[ELF_DATA] == ELF_LITTLE_ENDIAN) {
        return header[ELF_IDENT_SIZE] == ELF_EXEC && header[ELF_IDENT_SIZE + 1] == 0;
    }
    return header[ELF_DATA] == ELF_BIG_ENDIAN && header[ELF_IDENT_SIZE] == 0
        && header[ELF_IDENT_SIZE + 1] == ELF_EXEC;
}

/* Stream the immutable /bin directory instead of caching a truncated candidate
 * table. Work is linear in its finite entries; memory is independent of count.
 * Built-ins take precedence over same-named files. No fd survives a scan. */
static int scan_commands(const char *prefix, struct shell_matches *matches, int display) {
    for (size_t i = 0; i < sizeof(builtins) / sizeof(builtins[0]); i++) {
        if (shell_match_add(matches, prefix, builtins[i]) && display) {
            printf("%s  ", builtins[i]);
        }
    }
    int fd = open("/bin", O_RDONLY | O_DIRECTORY);
    if (fd < 0) return 0;
    int result = 0;
    for (;;) {
        long count = getdents64(fd, directory_entries, sizeof(directory_entries));
        if (count == 0) break;
        if (count < 0 || (size_t)count > sizeof(directory_entries)) {
            result = -1;
            break;
        }
        size_t offset = 0;
        while (offset < (size_t)count) {
            const size_t header = offsetof(struct genrt_dirent64, d_name);
            if ((size_t)count - offset < header + 1) { result = -1; break; }
            const struct genrt_dirent64 *entry =
                (const struct genrt_dirent64 *)(directory_entries + offset);
            if (entry->d_reclen < header + 1 || entry->d_reclen % 8 != 0
                || entry->d_reclen > (size_t)count - offset) { result = -1; break; }
            size_t capacity = entry->d_reclen - header;
            size_t length = strnlen(entry->d_name, capacity);
            if (length == capacity) { result = -1; break; }
            const char *name = entry->d_name;
            if (entry->d_type == DT_REG && length != 0
                && length + sizeof("/bin/") <= SHELL_PATH_MAX
                && strchr(name, ' ') == NULL && strchr(name, '\t') == NULL
                && !is_builtin(name) && strncmp(name, prefix, strlen(prefix)) == 0
                && is_command_image(name)
                && shell_match_add(matches, prefix, name) && display) {
                printf("%s  ", name);
            }
            offset += entry->d_reclen;
        }
        if (result != 0) break;
    }
    close(fd);
    return result;
}

static void show_prompt(void) {
    printf("\r\033[s> %s", input.line);
}

static void redraw_input(void) {
    // Restore the prompt's saved origin, including when a long line wrapped.
    // The editor owns the terminal area below that origin until submission.
    printf("\033[u\033[J> %s", input.line);
}

static void complete_input(void) {
    const char *prefix = shell_input_prefix(&input);
    if (prefix == NULL) return;
    struct shell_matches matches = {0};
    if (scan_commands(prefix, &matches, 0) != 0) return;
    if (shell_input_complete(&input, &matches)) {
        redraw_input();
    } else if (matches.count > 1) {
        printf("\n");
        struct shell_matches displayed = {0};
        scan_commands(prefix, &displayed, 1);
        printf("\n");
        show_prompt();
    }
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

    printf("%s", "genrt shell\n");

    for (;;) {
        shell_input_reset(&input);
        show_prompt();

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

            enum shell_input_action action = shell_input_feed(&input, (unsigned char)ch);
            if (action == SHELL_INPUT_SUBMIT) {
                printf("%s", "\n");
                break;
            }
            if (action == SHELL_INPUT_APPEND) {
                write(STDOUT_FILENO, &ch, 1);
            } else if (action == SHELL_INPUT_ERASE) {
                redraw_input();
            } else if (action == SHELL_INPUT_REDRAW) {
                redraw_input();
            } else if (action == SHELL_INPUT_COMPLETE) {
                complete_input();
            }
        }

        if (input.overflow) {
            printf("%s", "line too long\n");
            continue;
        }
        if (input.length == 0) {
            continue;
        }
        shell_input_remember(&input);
        run_command(input.line);
    }
}
