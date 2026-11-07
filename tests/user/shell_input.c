#include "../../user/c/shell_input.h"
#include "../../user/c/shell_command.h"
#include <stdio.h>
#include <unistd.h>

#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

static void type(struct shell_input *input, const char *text) {
    while (*text) shell_input_feed(input, (unsigned char)*text++);
}

static void set_line(struct shell_input *input, const char *text, size_t cursor) {
    shell_input_reset(input);
    input->length = strlen(text);
    memcpy(input->line, text, input->length + 1);
    input->cursor = cursor;
}

static void remember(struct shell_input *input, const char *line) {
    shell_input_reset(input);
    type(input, line);
    shell_input_remember(input);
}

static int history(void) {
    struct shell_input input = {0};
    type(&input, "draft\033[A\033[B");
    CHECK(strcmp(input.line, "draft") == 0);
    remember(&input, "");
    remember(&input, "   ");
    CHECK(input.count == 0);
    remember(&input, "first command");
    remember(&input, "second command");
    remember(&input, "second command");
    CHECK(input.count == 2);
    shell_input_reset(&input);
    type(&input, "unfinished\033[D\033[D\033[A");
    CHECK(strcmp(input.line, "second command") == 0);
    CHECK(input.cursor == input.length);
    type(&input, " edited\033[A");
    CHECK(strcmp(input.line, "first command") == 0);
    type(&input, "\033[A");
    CHECK(strcmp(input.line, "first command") == 0);
    type(&input, "\033[B");
    CHECK(strcmp(input.line, "second command") == 0);
    type(&input, "\033[B\033[B");
    CHECK(strcmp(input.line, "unfinished") == 0);
    CHECK(input.cursor == 8);
    // SS3 arrows work too; tokenization must not damage the ring entry.
    type(&input, "\033OA");
    input.line[6] = '\0';
    type(&input, "\033OB\033OA");
    CHECK(strcmp(input.line, "second command") == 0);

    for (unsigned i = 0; i < SHELL_HISTORY_CAPACITY + 3; i++) {
        char line[32];
        snprintf(line, sizeof(line), "command %u", i);
        remember(&input, line);
    }
    CHECK(input.count == SHELL_HISTORY_CAPACITY);
    shell_input_reset(&input);
    for (unsigned i = 0; i < SHELL_HISTORY_CAPACITY + 2; i++) type(&input, "\033[A");
    CHECK(strcmp(input.line, "command 3") == 0);
    for (unsigned i = 0; i < SHELL_HISTORY_CAPACITY; i++) type(&input, "\033[B");
    CHECK(input.line[0] == '\0');
    return 0;
}

static int editing(void) {
    struct shell_input input = {0};
    type(&input, "ac tail\033[D\033OD\033[D\033OD\033[D\033OD");
    CHECK(input.cursor == 1 && input.length == 7);
    CHECK(shell_input_feed(&input, 'b') == SHELL_INPUT_REDRAW);
    CHECK(strcmp(input.line, "abc tail") == 0 && input.cursor == 2);
    type(&input, "\b\033OC\177");
    CHECK(strcmp(input.line, "a tail") == 0 && input.cursor == 1);
    type(&input, "\033[D\033OD\b");
    CHECK(input.cursor == 0 && strcmp(input.line, "a tail") == 0);
    for (unsigned i = 0; i < 20; i++) type(&input, "\033[C\033OC");
    CHECK(input.cursor == input.length);
    type(&input, "\033[D\033[D");
    CHECK(shell_input_feed(&input, '\r') == SHELL_INPUT_SUBMIT);
    CHECK(strcmp(input.line, "a tail") == 0 && input.cursor == 4);
    CHECK(shell_input_feed(&input, '\n') == SHELL_INPUT_NONE);

    const char *deletes[] = {"\033\177", "\033\b"};
    for (size_t i = 0; i < 2; i++) {
        set_line(&input, "cat /one/two/ \t>>rest", 17);
        type(&input, deletes[i]);
        CHECK(strcmp(input.line, "cat /one/rest") == 0 && input.cursor == 9);
        type(&input, deletes[i]);
        CHECK(strcmp(input.line, "cat /rest") == 0 && input.cursor == 5);
        set_line(&input, " /\t>>suffix", 5);
        type(&input, deletes[i]);
        CHECK(strcmp(input.line, " /\t>>suffix") == 0 && input.cursor == 5);
        set_line(&input, "word suffix", 4);
        type(&input, deletes[i]);
        CHECK(strcmp(input.line, " suffix") == 0 && input.cursor == 0);
    }
    return 0;
}

static int terminal(void) {
    struct shell_input input = {0};
    type(&input, "abc\b\177d");
    CHECK(strcmp(input.line, "ad") == 0);
    type(&input, "\033[C\033[1;5A\033[999999999999999999999999999999~");
    CHECK(strcmp(input.line, "ad") == 0);
    type(&input, "\033x\033(B\001\037");
    CHECK(strcmp(input.line, "ad") == 0);
    CHECK(shell_input_feed(&input, 0xff) == SHELL_INPUT_APPEND);
    CHECK(shell_input_feed(&input, 0x7f) == SHELL_INPUT_ERASE);
    CHECK(strcmp(input.line, "ad") == 0);
    type(&input, "\033]0;discarded title\a\033Pdiscarded payload\033\\");
    CHECK(strcmp(input.line, "ad") == 0);
    type(&input, "\033[24;80");
    CHECK(shell_input_feed(&input, 'R') == SHELL_INPUT_GEOMETRY);
    CHECK(input.terminal_rows == 24 && input.terminal_columns == 80);
    type(&input, "\033[;80R\033[10000;80R\033[24;0R");
    CHECK(strcmp(input.line, "ad") == 0 && input.terminal_columns == 80);
    type(&input, "\033[");
    CHECK(shell_input_feed(&input, '\r') == SHELL_INPUT_SUBMIT);
    CHECK(strcmp(input.line, "ad") == 0);
    shell_input_reset(&input);
    CHECK(shell_input_feed(&input, '\n') == SHELL_INPUT_NONE);
    type(&input, "\033[\033[Zgood");
    CHECK(strcmp(input.line, "good") == 0);
    type(&input, "\033[");
    CHECK(shell_input_feed(&input, '\t') == SHELL_INPUT_COMPLETE);
    type(&input, "\033[");
    CHECK(shell_input_feed(&input, '\b') == SHELL_INPUT_ERASE);
    CHECK(strcmp(input.line, "goo") == 0);

    remember(&input, "saved");
    shell_input_reset(&input);
    for (unsigned i = 0; i < SHELL_LINE_MAX; i++) {
        CHECK(shell_input_feed(&input, 'x') == SHELL_INPUT_APPEND);
    }
    CHECK(input.length == SHELL_LINE_MAX && !input.overflow);
    CHECK(input.line[SHELL_LINE_MAX] == '\0');
    shell_input_feed(&input, 'y');
    CHECK(input.overflow && input.length == SHELL_LINE_MAX);
    shell_input_remember(&input);
    CHECK(input.count == 1);
    type(&input, "\033[D\033[D\033[A");
    CHECK(!input.overflow && strcmp(input.line, "saved") == 0);
    type(&input, "\033[B");
    CHECK(input.overflow && input.length == SHELL_LINE_MAX);
    CHECK(input.cursor == SHELL_LINE_MAX - 2);
    shell_input_feed(&input, 'z');
    CHECK(input.length == SHELL_LINE_MAX && input.cursor == SHELL_LINE_MAX - 2);
    type(&input, "\b");
    CHECK(input.overflow); // An overlong submission remains rejected.
    return 0;
}

static int completion(void) {
    struct shell_input input = {0};
    struct shell_completion context;
    struct shell_matches matches = {0};
    type(&input, "  ec");
    CHECK(shell_input_completion(&input, &context) && context.commands);
    CHECK(shell_completion_add(&matches, &input, &context, "echo", 0));
    CHECK(shell_input_complete(&input, &context, &matches));
    CHECK(strcmp(input.line, "  echo ") == 0);
    CHECK(shell_input_completion(&input, &context) && !context.commands);
    shell_input_reset(&input);
    type(&input, "g");
    CHECK(shell_input_completion(&input, &context));
    matches = (struct shell_matches){0};
    CHECK(shell_match_add(&matches, "g", "gtrt-a"));
    CHECK(shell_match_add(&matches, "g", "gtrt-b"));
    CHECK(matches.count == 2 && strcmp(matches.common, "gtrt-") == 0);
    CHECK(shell_input_complete(&input, &context, &matches));
    CHECK(strcmp(input.line, "gtrt-") == 0);
    CHECK(shell_input_completion(&input, &context));
    CHECK(!shell_input_complete(&input, &context, &matches)); // Caller displays candidates.
    matches = (struct shell_matches){0};
    CHECK(!shell_match_add(&matches, "gtrt-", "elsewhere"));
    CHECK(!shell_input_complete(&input, &context, &matches));
    CHECK(strcmp(input.line, "gtrt-") == 0);
    CHECK(!shell_match_add(&matches, "", "unsafe\033[A"));
    CHECK(!shell_match_add(&matches, "", "two words"));
    const char *unsafe[] = {"a<b", "a|b", "a&b", "a;b", "a\"b", "a'b", "a\\b", "a>b", "a/b"};
    for (size_t i = 0; i < sizeof(unsafe) / sizeof(unsafe[0]); i++) {
        CHECK(!shell_match_add(&matches, "", unsafe[i]));
    }

    const struct { const char *line; size_t cursor; const char *directory; const char *prefix; } paths[] = {
        {"/bin/ec", 7, "/bin/", "ec"}, {"echo argument", 13, ".", "argument"},
        {"echo>../ou", 10, "../", "ou"}, {"echo >>./ou", 11, "./", "ou"},
        {"./ec", 4, "./", "ec"}, {"cat /di/file tail", 7, "/", "di"},
    };
    for (size_t i = 0; i < sizeof(paths) / sizeof(paths[0]); i++) {
        set_line(&input, paths[i].line, paths[i].cursor);
        CHECK(shell_input_completion(&input, &context) && !context.commands);
        CHECK(strcmp(context.directory, paths[i].directory) == 0);
        CHECK(strcmp(context.prefix, paths[i].prefix) == 0);
    }
    set_line(&input, "cat /dir/file tail", 7);
    CHECK(shell_input_completion(&input, &context));
    matches = (struct shell_matches){0};
    CHECK(!shell_completion_add(&matches, &input, &context, "different", 1));
    CHECK(!shell_completion_add(&matches, &input, &context, "directory", 0));
    CHECK(shell_completion_add(&matches, &input, &context, "differ", 1));
    CHECK(shell_input_complete(&input, &context, &matches));
    CHECK(strcmp(input.line, "cat /differ/file tail") == 0 && input.cursor == 10);
    set_line(&input, "echo after", 2);
    CHECK(shell_input_completion(&input, &context) && context.commands);
    matches = (struct shell_matches){0};
    CHECK(shell_completion_add(&matches, &input, &context, "echo", 0));
    CHECK(!shell_input_complete(&input, &context, &matches)); // No duplicate ho or space.

    shell_input_reset(&input);
    for (unsigned i = 0; i < SHELL_LINE_MAX - 2; i++) type(&input, " ");
    type(&input, "ec");
    CHECK(shell_input_completion(&input, &context));
    matches = (struct shell_matches){0};
    shell_match_add(&matches, "ec", "echo");
    CHECK(!shell_input_complete(&input, &context, &matches));
    CHECK(input.length == SHELL_LINE_MAX && !input.overflow);
    type(&input, "\b\b\b\b");
    CHECK(shell_input_completion(&input, &context));
    CHECK(shell_input_complete(&input, &context, &matches));
    CHECK(input.length == SHELL_LINE_MAX); // Exact-fit match, no trailing space.
    CHECK(strcmp(input.line + SHELL_LINE_MAX - 4, "echo") == 0);
    return 0;
}

static int bounds(void) {
    struct shell_input input = {0};
    unsigned state = 12345;
    for (unsigned i = 0; i < 20000; i++) {
        state = state * 1664525u + 1013904223u;
        enum shell_input_action action = shell_input_feed(&input, (unsigned char)(state >> 24));
        if (action == SHELL_INPUT_SUBMIT) {
            shell_input_remember(&input);
            shell_input_reset(&input);
        } else if (action == SHELL_INPUT_COMPLETE) {
            struct shell_completion context;
            if (shell_input_completion(&input, &context)) {
                struct shell_matches matches = {0};
                shell_completion_add(&matches, &input, &context, "cd", 0);
                shell_completion_add(&matches, &input, &context, "exit", 0);
                shell_input_complete(&input, &context, &matches);
            }
        }
        CHECK(input.length <= SHELL_LINE_MAX && input.line[input.length] == '\0');
        CHECK(input.cursor <= input.length);
        CHECK(input.count <= SHELL_HISTORY_CAPACITY && input.age <= input.count);
        CHECK(input.next < SHELL_HISTORY_CAPACITY && input.escape_bytes <= SHELL_ESCAPE_MAX);
        for (unsigned slot = 0; slot < SHELL_HISTORY_CAPACITY; slot++) {
            CHECK(strnlen(input.history[slot], SHELL_LINE_MAX + 1) <= SHELL_LINE_MAX);
        }
    }
    return 0;
}

static int parsing(void) {
    struct shell_command command;
    CHECK(shell_parse("  echo one\ttwo> /mnt/out  ", &command) == SHELL_PARSE_OK);
    CHECK(command.argc == 3 && command.argv[3] == NULL);
    CHECK(strcmp(command.argv[0], "echo") == 0 && strcmp(command.argv[2], "two") == 0);
    CHECK(strcmp(command.output, "/mnt/out") == 0 && !command.append);
    CHECK(shell_parse("echo>>file after", &command) == SHELL_PARSE_OK);
    CHECK(command.argc == 2 && strcmp(command.argv[1], "after") == 0 && command.append);
    CHECK(strcmp(command.output, "file") == 0);
    CHECK(shell_parse("echo 2 >file", &command) == SHELL_PARSE_OK);
    const char *invalid[] = {">file", "echo >", "echo >>>file", "echo >a >b", "echo 2>file",
        "echo 2>>file", "echo > >file", "echo <file", "echo | cat", "echo 'quoted'",
        "echo \"quoted\"", "echo >file &", "echo;exit", "echo >file\\name"};
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); i++) {
        CHECK(shell_parse(invalid[i], &command) == SHELL_PARSE_SYNTAX);
    }
    CHECK(shell_parse("a b c d e f g h i j k l m n o p >out", &command) == SHELL_PARSE_OK);
    CHECK(command.argc == SHELL_ARG_MAX && command.argv[SHELL_ARG_MAX] == NULL);
    CHECK(shell_parse("a b c d e f g h i j k l m n o p q >out", &command) == SHELL_PARSE_TOO_MANY_ARGS);
    char maximum[SHELL_LINE_MAX + 1];
    memset(maximum, 'x', SHELL_LINE_MAX);
    maximum[SHELL_LINE_MAX] = '\0';
    CHECK(shell_parse(maximum, &command) == SHELL_PARSE_OK);
    CHECK(strlen(command.argv[0]) == SHELL_LINE_MAX);
    return 0;
}

int main(void) {
    int line = history();
    if (!line) line = editing();
    if (!line) line = terminal();
    if (!line) line = completion();
    if (!line) line = bounds();
    if (!line) line = parsing();
    if (line) dprintf(STDERR_FILENO, "shell input failed at line %d\n", line);
    return line != 0;
}
