/* Execute the production shell command path with controlled child syscalls. */
#include <setjmp.h>
#define main shell_program_main
#include "../../user/c/shell.c"
#undef main

#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

static jmp_buf child_exit;
static int fork_result, open_result, dup_result, forks, opens, duplicates, closes, executions, waits;
static int exit_code, opened_flags, closed_fd, source_fd, target_fd, error_fd;
static int exec_result;
static char opened_path[PATH_MAX], executed_path[128], argument[128];

static void reset(void) {
    fork_result = 0;
    open_result = 3;
    dup_result = 1;
    forks = opens = duplicates = closes = executions = waits = 0;
    exit_code = -1;
    exec_result = 0;
    opened_flags = closed_fd = source_fd = target_fd = error_fd = -1;
    opened_path[0] = executed_path[0] = argument[0] = '\0';
}

int shell_test_open(const char *path, int flags) {
    opens++;
    opened_flags = flags;
    snprintf(opened_path, sizeof(opened_path), "%s", path);
    return open_result;
}
ssize_t shell_test_read(int fd, void *buffer, size_t count) {
    (void)fd; (void)buffer; (void)count;
    return 0;
}
ssize_t shell_test_write(int fd, const void *buffer, size_t count) {
    (void)fd; (void)buffer;
    return (ssize_t)count;
}
int shell_test_close(int fd) { closes++; closed_fd = fd; return 0; }
int shell_test_dup2(int fd, int target) {
    duplicates++; source_fd = fd; target_fd = target;
    return dup_result;
}
pid_t shell_test_fork(void) { forks++; return fork_result; }
int shell_test_execve(const char *path, char *const argv[], char *const envp[]) {
    (void)envp;
    executions++;
    snprintf(executed_path, sizeof(executed_path), "%s", path);
    if (argv[1]) snprintf(argument, sizeof(argument), "%s", argv[1]);
    if (exec_result < 0) return exec_result;
    // A successful exec replaces this child; longjmp prevents returning into it.
    longjmp(child_exit, 1);
}
pid_t shell_test_waitpid(pid_t pid, int *status, int options) {
    (void)options; waits++; *status = 0; return pid;
}
int shell_test_chdir(const char *path) { (void)path; return 0; }
long shell_test_getdents64(int fd, void *buffer, size_t count) {
    (void)fd; (void)buffer; (void)count; return 0;
}
__attribute__((noreturn)) void shell_test_exit(int code) { exit_code = code; longjmp(child_exit, 1); }
int shell_test_printf(const char *format, ...) { (void)format; return 0; }
int shell_test_dprintf(int fd, const char *format, ...) { error_fd = fd; (void)format; return 0; }

static void command(const char *text) {
    char line[SHELL_LINE_MAX + 1];
    snprintf(line, sizeof(line), "%s", text);
    if (setjmp(child_exit) == 0) run_command(line);
}

int main(void) {
    reset();
    fork_result = 42;
    command("echo parent >file");
    CHECK(forks == 1 && waits == 1 && opens == 0 && duplicates == 0 && closes == 0);
    reset();
    command("echo child>file");
    CHECK(forks == 1 && opens == 1 && duplicates == 1 && executions == 1 && waits == 0);
    CHECK(opened_flags == (O_WRONLY | O_CREAT | O_TRUNC));
    CHECK(strcmp(opened_path, "file") == 0 && strcmp(executed_path, "/bin/echo") == 0);
    CHECK(strcmp(argument, "child") == 0 && source_fd == 3 && target_fd == STDOUT_FILENO);
    CHECK(closes == 1 && closed_fd == 3 && exit_code == -1);
    reset();
    open_result = STDOUT_FILENO;
    command("echo child >> file");
    CHECK(opened_flags == (O_WRONLY | O_CREAT | O_APPEND));
    CHECK(duplicates == 1 && source_fd == 1 && target_fd == 1 && closes == 0 && executions == 1);
    reset();
    open_result = -30;
    command("echo child >readonly");
    CHECK(opens == 1 && duplicates == 0 && executions == 0 && closes == 0 && exit_code == 126);
    CHECK(error_fd == STDERR_FILENO);
    reset();
    dup_result = -9;
    command("echo child >file");
    CHECK(opens == 1 && duplicates == 1 && closes == 1 && closed_fd == 3);
    CHECK(executions == 0 && exit_code == 126 && error_fd == STDERR_FILENO);
    reset();
    exec_result = -2;
    command("missing >file");
    CHECK(executions == 1 && closes == 1 && closed_fd == 3 && exit_code == 127);
    CHECK(error_fd == STDERR_FILENO);
    const char *rejected[] = {"cd >file", "exit >>file", "/etc/banner >file", "echo >", "echo >a >b", "echo 2>file"};
    for (size_t i = 0; i < sizeof(rejected) / sizeof(rejected[0]); i++) {
        reset(); command(rejected[i]);
        CHECK(forks == 0 && opens == 0 && executions == 0 && error_fd == STDERR_FILENO);
    }
    reset();
    fork_result = -12;
    command("echo child >file");
    CHECK(forks == 1 && opens == 0 && waits == 0);
    return 0;
}
