#include "syscall.h"
#include <unistd.h>
#include <stdio.h>
#include <string.h>
#include "shell_input.h"
#include "shell_command.h"

#define SHELL_PATH_MAX 128

static const char *const builtins[] = {"cd", "exit"};
static struct shell_input input;
static unsigned terminal_columns = 80;
static int terminal_query;
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

/* Stream entries without a candidate-count cap. Bare commands retain builtin
 * precedence and ELF filtering; paths use directory kind, never stat or ELF.
 * At most one directory and one candidate-image fd are open; none escape. */
static int scan_completion(const struct shell_completion *completion,
                           struct shell_matches *matches,
                           void (*visit)(const char *name, int directory)) {
    if (completion->commands) {
        for (size_t i = 0; i < sizeof(builtins) / sizeof(builtins[0]); i++) {
            if (shell_completion_add(matches, &input, completion, builtins[i], 0) && visit) {
                visit(builtins[i], 0);
            }
        }
    }
    int fd = open(completion->commands ? "/bin" : completion->directory,
                  O_RDONLY | O_DIRECTORY);
    if (fd < 0) return completion->commands ? 0 : -1;
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
            int directory = entry->d_type == DT_DIR;
            int usable = length != 0 && length <= SHELL_LINE_MAX
                && (directory || entry->d_type == DT_REG)
                && strcmp(name, ".") != 0 && strcmp(name, "..") != 0;
            for (size_t i = 0; usable && i < length; i++) {
                if (!shell_name_byte((unsigned char)name[i])) usable = 0;
            }
            if (usable && completion->commands) {
                usable = !directory && length + sizeof("/bin/") <= SHELL_PATH_MAX
                    && !is_builtin(name)
                    && strncmp(name, completion->prefix, strlen(completion->prefix)) == 0
                    && is_command_image(name);
            }
            if (usable && shell_completion_add(matches, &input, completion, name, directory)
                && visit) {
                visit(name, directory);
            }
            offset += entry->d_reclen;
        }
        if (result != 0) break;
    }
    close(fd);
    return result;
}

/* Replay a bounded prefix from the refreshed prompt origin. */
static void position_input(size_t cursor) {
    char prefix[SHELL_LINE_MAX + 1];
    memcpy(prefix, input.line, cursor);
    prefix[cursor] = '\0';
    printf("\033[u> %s", prefix);
}

static void redraw_input(void) {
    // Reserve wrapped rows before drawing, then save the possibly scrolled
    // origin again. A saved screen coordinate alone does not track scrolling.
    size_t rows = (2 + input.length) / terminal_columns;
    printf("\033[u\r");
    for (size_t row = 0; row < rows; row++) printf("\r\n");
    if (rows) printf("\033[%zuA", rows);
    printf("\r\033[s\033[J> %s", input.line);
    if (input.cursor != input.length) position_input(input.cursor);
}

static void show_prompt(void) {
    printf("\r\033[s");
    redraw_input();
}

static void finish_input(void) {
    position_input(input.length);
    printf("%s", "\r\n");
}

static void show_candidate(const char *name, int directory) {
    printf("%s%s  ", name, directory ? "/" : "");
}

static void complete_input(void) {
    struct shell_completion completion;
    if (!shell_input_completion(&input, &completion)) return;
    struct shell_matches matches = {0};
    if (scan_completion(&completion, &matches, NULL) != 0) return;
    if (shell_input_complete(&input, &completion, &matches)) {
        redraw_input();
    } else if (matches.count > 1) {
        finish_input();
        struct shell_matches displayed = {0};
        scan_completion(&completion, &displayed, show_candidate);
        printf("\n");
        show_prompt();
    }
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
    struct shell_command command;
    char path[SHELL_PATH_MAX];
    enum shell_parse_result parsed = shell_parse(line, &command);
    int argc = command.argc;
    char **argv = command.argv;

    if (parsed != SHELL_PARSE_OK) {
        dprintf(STDERR_FILENO, "%s", parsed == SHELL_PARSE_TOO_MANY_ARGS
                ? "too many args\n" : "redirection: malformed or unsupported syntax\n");
        return;
    }
    if (argc == 0) {
        return;
    }
    int print_path = argv[0][0] == '/' && argc == 1 && strncmp(argv[0], "/bin/", 5) != 0;
    if (command.output && (is_builtin(argv[0]) || print_path)) {
        dprintf(STDERR_FILENO, "%s", "redirection: unsupported for built-in command\n");
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

    if (print_path) {
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
        if (command.output) {
            int flags = O_WRONLY | O_CREAT | (command.append ? O_APPEND : O_TRUNC);
            int fd = open(command.output, flags);
            if (fd < 0) {
                dprintf(STDERR_FILENO, "redirection: open failed (%d)\n", fd);
                exit(126);
            }
            int result = dup2(fd, STDOUT_FILENO);
            if (fd != STDOUT_FILENO) close(fd);
            if (result < 0) {
                dprintf(STDERR_FILENO, "redirection: dup2 failed (%d)\n", result);
                exit(126);
            }
        }
        execve(path, argv, NULL);
        dprintf(STDERR_FILENO, "%s", "exec failed\n");
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
    // A raw UART needs no TTY ioctl. Standard ANSI terminals report the
    // clamped bottom-right position; headless/nonresponding peers use 80 cols.
    terminal_query = 1;
    printf("\033[s\033[999;999H\033[6n\033[u");

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
            if (action == SHELL_INPUT_GEOMETRY) {
                if (terminal_query) {
                    terminal_columns = input.terminal_columns;
                    terminal_query = 0;
                    redraw_input();
                }
                continue;
            }
            if (action != SHELL_INPUT_NONE) terminal_query = 0;
            if (action == SHELL_INPUT_SUBMIT) {
                finish_input();
                break;
            }
            if (action == SHELL_INPUT_APPEND) {
                if ((2 + input.length) % terminal_columns == 0) redraw_input();
                else write(STDOUT_FILENO, &ch, 1);
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
