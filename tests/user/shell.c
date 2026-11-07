/* Execute the production shell command path with controlled child syscalls. */
#include <setjmp.h>
#include <stdarg.h>
#define main shell_program_main
#include "../../user/c/shell.c"
#undef main

#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

static jmp_buf child_exit;
static int fork_result, open_result, dup_result, forks, opens, duplicates, closes, executions, waits;
static int exit_code, opened_flags, closed_fd, source_fd, target_fd, error_fd;
static int exec_result;
static char opened_path[PATH_MAX], executed_path[128], argument[128];
static char output[32768];
static size_t output_length;

/* One record per getdents call forces streaming. The fixture stays bounded,
 * while allowing enough entries to catch a candidate-table truncation. */
static struct {
    char name[SHELL_LINE_MAX + 2];
    unsigned char kind;
    int image;
} entries[128];
static size_t entry_count, entry_offset, image_offset, image_entry;
static const char *fixture_directory;
static int fixture_mode, directory_live, image_live, directory_fd;
static int scan_fault, image_fault, fd_violation, getdents_calls;

static void reset(void) {
    fork_result = 0;
    open_result = 3;
    dup_result = 1;
    forks = opens = duplicates = closes = executions = waits = 0;
    exit_code = -1;
    exec_result = 0;
    opened_flags = closed_fd = source_fd = target_fd = error_fd = -1;
    opened_path[0] = executed_path[0] = argument[0] = '\0';
    output[0] = '\0';
    output_length = 0;
    fixture_mode = directory_live = image_live = 0;
    entry_count = entry_offset = image_offset = 0;
    scan_fault = image_fault = fd_violation = getdents_calls = 0;
    directory_fd = 3;
    terminal_columns = 80;
    terminal_query = 0;
}

int shell_test_open(const char *path, int flags) {
    opens++;
    opened_flags = flags;
    snprintf(opened_path, sizeof(opened_path), "%s", path);
    if (fixture_mode) {
        if (flags & O_DIRECTORY) {
            if (open_result < 0) return open_result;
            if (strcmp(path, fixture_directory) != 0 || directory_live) {
                fd_violation = 1;
                return -2;
            }
            directory_live = 1;
            entry_offset = 0;
            return directory_fd;
        }
        for (size_t i = 0; i < entry_count; i++) {
            if (strncmp(path, "/bin/", 5) == 0 && strcmp(path + 5, entries[i].name) == 0) {
                if (image_live) fd_violation = 1;
                if (entries[i].image < 0) return -2;
                image_live = 1;
                image_entry = i;
                image_offset = 0;
                return 4;
            }
        }
        fd_violation = 1;
        return -2;
    }
    return open_result;
}
ssize_t shell_test_read(int fd, void *buffer, size_t count) {
    if (fixture_mode && image_live && fd == 4) {
        unsigned char elf[18] = {0x7f, 'E', 'L', 'F', 2, 1};
        if (image_fault) return -5;
        if (entries[image_entry].image == 1) elf[16] = 2;
        else if (entries[image_entry].image == 2) { elf[5] = 2; elf[17] = 2; }
        else elf[16] = 3; // Non-ET_EXEC files must not become bare commands.
        if (image_offset == sizeof(elf)) return 0;
        if (count > 3) count = 3; // Verify short ELF reads as well.
        if (count > sizeof(elf) - image_offset) count = sizeof(elf) - image_offset;
        memcpy(buffer, elf + image_offset, count);
        image_offset += count;
        return (ssize_t)count;
    }
    return 0;
}
ssize_t shell_test_write(int fd, const void *buffer, size_t count) {
    (void)fd; (void)buffer;
    return (ssize_t)count;
}
int shell_test_close(int fd) {
    closes++;
    closed_fd = fd;
    if (fixture_mode) {
        if (fd == directory_fd && directory_live) directory_live = 0;
        else if (fd == 4 && image_live) image_live = 0;
        else fd_violation = 1;
    }
    return 0;
}
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
    getdents_calls++;
    if (!fixture_mode) return 0;
    if (fd != directory_fd || !directory_live) { fd_violation = 1; return -9; }
    if (scan_fault && entry_offset == 1) {
        if (scan_fault == 1) return -5;
        if (scan_fault == 2) return (long)count + 1;
        struct genrt_dirent64 *entry = buffer;
        memset(buffer, 'x', 32);
        entry->d_reclen = scan_fault == 3 ? 7 : 32;
        return 32; // Bad framing or missing terminating NUL.
    }
    if (entry_offset == entry_count) return 0;
    size_t size = (offsetof(struct genrt_dirent64, d_name)
                   + strlen(entries[entry_offset].name) + 1 + 7) & ~(size_t)7;
    if (size > count) return -22;
    memset(buffer, 0, size);
    struct genrt_dirent64 *entry = buffer;
    entry->d_reclen = (unsigned short)size;
    entry->d_type = entries[entry_offset].kind;
    memcpy(entry->d_name, entries[entry_offset].name, strlen(entries[entry_offset].name) + 1);
    entry_offset++;
    return (long)size;
}
__attribute__((noreturn)) void shell_test_exit(int code) { exit_code = code; longjmp(child_exit, 1); }
int shell_test_printf(const char *format, ...) {
    va_list args;
    va_start(args, format);
    int count = vsnprintf(output + output_length, sizeof(output) - output_length, format, args);
    va_end(args);
    if (count < 0 || (size_t)count >= sizeof(output) - output_length) {
        fd_violation = 1;
        return -1;
    }
    output_length += (size_t)count;
    return count;
}
int shell_test_dprintf(int fd, const char *format, ...) { error_fd = fd; (void)format; return 0; }

static void command(const char *text) {
    char line[SHELL_LINE_MAX + 1];
    snprintf(line, sizeof(line), "%s", text);
    if (setjmp(child_exit) == 0) run_command(line);
}

static int commands(void) {
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

static void fixture(const char *directory) {
    reset();
    fixture_mode = 1;
    fixture_directory = directory;
}

static void add_entry(const char *name, unsigned char kind, int image) {
    if (entry_count == sizeof(entries) / sizeof(entries[0])) { fd_violation = 1; return; }
    snprintf(entries[entry_count].name, sizeof(entries[entry_count].name), "%s", name);
    entries[entry_count].kind = kind;
    entries[entry_count++].image = image;
}

static void edit_line(const char *text, size_t cursor) {
    input = (struct shell_input){0};
    input.length = strlen(text);
    memcpy(input.line, text, input.length + 1);
    input.cursor = cursor;
}

static int contains(const char *text) {
    for (size_t i = 0; i < output_length; i++) {
        if (strncmp(output + i, text, strlen(text)) == 0) return 1;
    }
    return 0;
}

static int paths(void) {
    const struct { const char *line; size_t cursor; const char *directory; const char *expected; } cases[] = {
        {"cat /fi", 7, "/", "cat /file.txt "},
        {"cat fi", 6, ".", "cat file.txt "},
        {"cat ./fi", 8, "./", "cat ./file.txt "},
        {"cat ../fi", 9, "../", "cat ../file.txt "},
        {"./fi", 4, "./", "./file.txt "},
        {"/fi", 3, "/", "/file.txt "},
        {"echo>/fi", 8, "/", "echo>/file.txt "},
        {"echo >>./fi", 11, "./", "echo >>./file.txt "},
        {"cat /di", 7, "/", "cat /directory/"},
        {"cat /di/file.txt next", 7, "/", "cat /directory/file.txt next"},
        {"cat /direcory/file.txt next", 10, "/", "cat /directory/file.txt next"},
        {"cat /fi.txt tail", 7, "/", "cat /file.txt tail"},
        {"cat /fi tail", 7, "/", "cat /file.txt tail"},
        {"cat /fi>out", 7, "/", "cat /file.txt>out"},
    };
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); i++) {
        fixture(cases[i].directory);
        add_entry("file.txt", DT_REG, 0); // Path completion does not inspect ELF.
        add_entry("directory", DT_DIR, 0);
        add_entry(".", DT_DIR, 0);
        add_entry("..", DT_DIR, 0);
        add_entry("fifo", 1, 0);
        add_entry("file unsafe", DT_REG, 0);
        add_entry("file>unsafe", DT_REG, 0);
        edit_line(cases[i].line, cases[i].cursor);
        complete_input();
        CHECK(strcmp(input.line, cases[i].expected) == 0);
        CHECK(input.cursor <= input.length && !input.overflow);
        CHECK(opens == 1 && closes == 1 && !directory_live && !image_live && !fd_violation);
    }
    fixture(".");
    add_entry("alpha", DT_REG, 0);
    add_entry("alpine", DT_DIR, 0);
    edit_line("cat a", 5);
    complete_input();
    CHECK(strcmp(input.line, "cat alp") == 0 && !contains("alpha  "));
    complete_input();
    CHECK(strcmp(input.line, "cat alp") == 0 && contains("alpha  alpine/  "));
    CHECK(opens == closes && !fd_violation && !directory_live);
    output_length = 0; output[0] = '\0';
    edit_line("cat missing tail", 11);
    complete_input();
    CHECK(strcmp(input.line, "cat missing tail") == 0 && input.cursor == 11 && !output_length);
    CHECK(opens == closes && !fd_violation);

    fixture(".");
    char oversized[SHELL_LINE_MAX + 2];
    memset(oversized, 'x', sizeof(oversized) - 1);
    oversized[sizeof(oversized) - 1] = '\0';
    add_entry(oversized, DT_REG, 0);
    edit_line("cat x", 5);
    complete_input();
    CHECK(strcmp(input.line, "cat x") == 0 && !output_length && opens == closes);

    fixture(".");
    add_entry("file.txt", DT_REG, 0);
    char full[SHELL_LINE_MAX + 1];
    memset(full, ' ', sizeof(full) - 1);
    memcpy(full, "cat fi", 6);
    full[SHELL_LINE_MAX] = '\0';
    edit_line(full, 6);
    complete_input();
    CHECK(strcmp(input.line, full) == 0 && input.cursor == 6 && !input.overflow);
    return 0;
}

static int command_completion(void) {
    fixture("/bin");
    add_entry("echo", DT_REG, 1);
    add_entry("cd", DT_REG, 1); // Must not duplicate or inspect the builtin.
    add_entry("exit", DT_REG, 1);
    add_entry("else", DT_REG, 0);
    add_entry("entry", DT_DIR, 1);
    add_entry("evil;name", DT_REG, 1);
    edit_line("ec tail", 2);
    complete_input();
    CHECK(strcmp(input.line, "echo tail") == 0 && input.cursor == 4);
    CHECK(opens == 2 && closes == 2 && !image_live && !directory_live && !fd_violation);
    edit_line("c", 1);
    complete_input();
    CHECK(strcmp(input.line, "cd ") == 0 && opens == 3 && closes == 3);
    edit_line("el", 2);
    complete_input();
    CHECK(strcmp(input.line, "el") == 0 && !image_live && opens == closes);

    fixture("/bin");
    add_entry("big-endian", DT_REG, 2);
    edit_line("big", 3);
    complete_input();
    CHECK(strcmp(input.line, "big-endian ") == 0 && opens == closes && !fd_violation);
    fixture("/bin");
    for (unsigned i = 0; i < 100; i++) {
        char name[32];
        snprintf(name, sizeof(name), "entry%u", i);
        add_entry(name, DT_REG, 1);
    }
    add_entry("zz-last", DT_REG, 1);
    edit_line("zz", 2);
    complete_input();
    CHECK(strcmp(input.line, "zz-last ") == 0 && getdents_calls == 102);
    CHECK(opens == closes && !image_live && !directory_live && !fd_violation);
    return 0;
}

static int scan_errors(void) {
    for (int fault = 1; fault <= 4; fault++) {
        fixture(".");
        add_entry("file", DT_REG, 0);
        add_entry("final", DT_REG, 0);
        scan_fault = fault;
        edit_line("cat fi", 6);
        complete_input();
        CHECK(strcmp(input.line, "cat fi") == 0 && !output_length);
        CHECK(opens == closes && closes == 1 && !directory_live && !fd_violation);
    }
    fixture(".");
    open_result = -2;
    edit_line("cat fi", 6);
    complete_input();
    CHECK(strcmp(input.line, "cat fi") == 0 && opens == 1 && closes == 0 && !output_length);
    fixture("/bin");
    open_result = -2;
    edit_line("c", 1);
    complete_input();
    CHECK(strcmp(input.line, "cd ") == 0 && closes == 0);
    for (int fault = 0; fault < 2; fault++) {
        fixture("/bin");
        add_entry("echo", DT_REG, fault ? 1 : -1);
        image_fault = fault;
        edit_line("ec", 2);
        complete_input();
        CHECK(strcmp(input.line, "ec") == 0 && !output_length);
        CHECK(closes == 1 + fault && !image_live && !directory_live && !fd_violation);
    }
    fixture(".");
    directory_fd = 0; // Descriptor zero is a valid successful open.
    add_entry("file", DT_REG, 0);
    edit_line("cat fi", 6);
    complete_input();
    CHECK(strcmp(input.line, "cat file ") == 0 && closed_fd == 0 && !directory_live && !fd_violation);
    return 0;
}

/* Only the emitted save/restore/erase controls, wrapping, CR and newline are
 * modeled. This is a rendering oracle, not a general terminal emulator. */
struct terminal {
    char cells[80 * 24];
    size_t cursor, saved, width, height, scrolls;
    int pending_wrap;
};

static void terminal_scroll(struct terminal *terminal) {
    size_t size = terminal->width * terminal->height;
    if (terminal->cursor >= size) {
        memmove(terminal->cells, terminal->cells + terminal->width, size - terminal->width);
        memset(terminal->cells + size - terminal->width, ' ', terminal->width);
        terminal->cursor -= terminal->width;
        terminal->scrolls++;
        // Saved cursor coordinates do not follow the scrolled contents.
    }
}

static int render(struct terminal *terminal) {
    size_t size = terminal->width * terminal->height;
    CHECK(size <= sizeof(terminal->cells));
    for (size_t i = 0; i < output_length; i++) {
        char ch = output[i];
        if (ch == '\033') {
            CHECK(i + 2 < output_length && output[i + 1] == '[');
            i += 2;
            unsigned count = 0;
            while (i < output_length && output[i] >= '0' && output[i] <= '9')
                count = count * 10 + (unsigned)(output[i++] - '0');
            CHECK(i < output_length);
            ch = output[i];
            if (ch == 's') terminal->saved = terminal->cursor;
            else if (ch == 'u') terminal->cursor = terminal->saved;
            else if (ch == 'J') memset(terminal->cells + terminal->cursor, ' ',
                                       size - terminal->cursor);
            else if (ch == 'A') {
                size_t rows = terminal->cursor / terminal->width;
                if (count > rows) count = (unsigned)rows;
                terminal->cursor -= count * terminal->width;
            }
            else CHECK(0);
            terminal->pending_wrap = 0;
        } else if (ch == '\r') {
            terminal->cursor -= terminal->cursor % terminal->width;
            terminal->pending_wrap = 0;
        } else if (ch == '\n') {
            terminal->cursor += terminal->width;
            terminal->pending_wrap = 0;
            terminal_scroll(terminal);
        }
        else {
            if (terminal->pending_wrap) {
                terminal->cursor++;
                terminal_scroll(terminal);
                terminal->pending_wrap = 0;
            }
            CHECK(terminal->cursor < size);
            terminal->cells[terminal->cursor] = ch;
            if (terminal->cursor % terminal->width == terminal->width - 1)
                terminal->pending_wrap = 1;
            else terminal->cursor++;
        }
        CHECK(terminal->cursor < size);
    }
    output_length = 0; output[0] = '\0';
    return 0;
}

static int rendering(void) {
    for (size_t width = 12; width <= 80; width += 68) {
      for (unsigned bottom = 0; bottom < 2; bottom++) {
        fixture(".");
        terminal_columns = (unsigned)width;
        struct terminal terminal = {.width = width, .height = 24};
        memset(terminal.cells, ' ', sizeof(terminal.cells));
        if (bottom) terminal.cursor = (terminal.height - 1) * width;
        char long_line[102];
        memset(long_line, 'a', sizeof(long_line) - 1);
        memcpy(long_line, "cat ", 4);
        long_line[100] = 'Z';
        long_line[101] = '\0';
        edit_line(long_line, 10);
        show_prompt();
        CHECK(render(&terminal) == 0);
        CHECK(terminal.cursor + terminal.pending_wrap == terminal.saved + 2 + input.cursor);
        CHECK(memcmp(terminal.cells + terminal.saved + 2, input.line, input.length) == 0);
        size_t previous = input.length;
        shell_input_feed(&input, '\b');
        redraw_input();
        CHECK(render(&terminal) == 0);
        CHECK(terminal.cursor + terminal.pending_wrap == terminal.saved + 2 + input.cursor);
        CHECK(memcmp(terminal.cells + terminal.saved + 2, input.line, input.length) == 0);
        CHECK(terminal.cells[terminal.saved + 2 + previous - 1] == ' ');
        shell_input_feed(&input, 'X');
        redraw_input();
        CHECK(render(&terminal) == 0);
        CHECK(terminal.cursor + terminal.pending_wrap == terminal.saved + 2 + input.cursor);
        CHECK(memcmp(terminal.cells + terminal.saved + 2, input.line, input.length) == 0);
        size_t origin = terminal.saved, scrolls = terminal.scrolls;
        finish_input();
        CHECK(render(&terminal) == 0 && terminal.cursor % width == 0);
        origin -= (terminal.scrolls - scrolls) * width;
        CHECK(memcmp(terminal.cells + origin + 2, input.line, input.length) == 0);

        edit_line(long_line, strlen(long_line));
        show_prompt();
        CHECK(render(&terminal) == 0);
        shell_input_feed(&input, '\033');
        shell_input_feed(&input, 0x7f);
        redraw_input();
        CHECK(render(&terminal) == 0 && strcmp(input.line, "cat ") == 0);
        CHECK(memcmp(terminal.cells + terminal.saved + 2, "cat ", 4) == 0);
        CHECK(terminal.cells[terminal.saved + 2 + previous - 1] == ' ');
        finish_input();
        CHECK(render(&terminal) == 0);

        edit_line("cat alp subsequent-long-argument", 7);
        add_entry("alpha", DT_REG, 0);
        add_entry("alpine", DT_DIR, 0);
        show_prompt();
        CHECK(render(&terminal) == 0);
        origin = terminal.saved;
        scrolls = terminal.scrolls;
        complete_input(); // Cannot extend; listing must start below the full suffix.
        CHECK(contains("alpha  alpine/  "));
        CHECK(render(&terminal) == 0);
        origin -= (terminal.scrolls - scrolls) * width;
        CHECK(memcmp(terminal.cells + origin + 2, input.line, input.length) == 0);
        CHECK(terminal.saved > origin + input.length);
        CHECK(terminal.cursor + terminal.pending_wrap == terminal.saved + 2 + input.cursor);
        CHECK(memcmp(terminal.cells + terminal.saved + 2, input.line, input.length) == 0);
        CHECK(!fd_violation && opens == closes);
      }
    }
    return 0;
}

#undef dprintf
int main(void) {
    int line = commands();
    if (!line) line = paths();
    if (!line) line = command_completion();
    if (!line) line = scan_errors();
    if (!line) line = rendering();
    if (line) dprintf(STDERR_FILENO, "shell failed at line %d\n", line);
    return line != 0;
}
