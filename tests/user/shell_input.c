#include "../../user/c/shell_input.h"
#include <stdio.h>
#include <unistd.h>

#define CHECK(condition) do { if (!(condition)) return __LINE__; } while (0)

static void type(struct shell_input *input, const char *text) {
    while (*text) shell_input_feed(input, (unsigned char)*text++);
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
    type(&input, "unfinished\033[A");
    CHECK(strcmp(input.line, "second command") == 0);
    type(&input, " edited\033[A");
    CHECK(strcmp(input.line, "first command") == 0);
    type(&input, "\033[A");
    CHECK(strcmp(input.line, "first command") == 0);
    type(&input, "\033[B");
    CHECK(strcmp(input.line, "second command") == 0);
    type(&input, "\033[B\033[B");
    CHECK(strcmp(input.line, "unfinished") == 0);
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
    type(&input, "\033[A");
    CHECK(!input.overflow && strcmp(input.line, "saved") == 0);
    type(&input, "\033[B");
    CHECK(input.overflow && input.length == SHELL_LINE_MAX);
    type(&input, "\b");
    CHECK(input.overflow); // An overlong submission remains rejected.
    return 0;
}

static int completion(void) {
    struct shell_input input = {0};
    struct shell_matches matches = {0};
    type(&input, "  ec");
    CHECK(shell_match_add(&matches, shell_input_prefix(&input), "echo"));
    CHECK(shell_input_complete(&input, &matches));
    CHECK(strcmp(input.line, "  echo ") == 0);
    CHECK(shell_input_prefix(&input) == NULL);
    shell_input_reset(&input);
    type(&input, "g");
    matches = (struct shell_matches){0};
    CHECK(shell_match_add(&matches, "g", "gtrt-a"));
    CHECK(shell_match_add(&matches, "g", "gtrt-b"));
    CHECK(matches.count == 2 && strcmp(matches.common, "gtrt-") == 0);
    CHECK(shell_input_complete(&input, &matches));
    CHECK(strcmp(input.line, "gtrt-") == 0);
    CHECK(!shell_input_complete(&input, &matches)); // Caller displays candidates.
    matches = (struct shell_matches){0};
    CHECK(!shell_match_add(&matches, "gtrt-", "elsewhere"));
    CHECK(!shell_input_complete(&input, &matches));
    CHECK(strcmp(input.line, "gtrt-") == 0);
    CHECK(!shell_match_add(&matches, "", "unsafe\033[A"));
    CHECK(!shell_match_add(&matches, "", "two words"));

    shell_input_reset(&input);
    type(&input, "/bin/ec");
    CHECK(shell_input_prefix(&input) == NULL);
    shell_input_reset(&input);
    type(&input, "echo argument");
    CHECK(shell_input_prefix(&input) == NULL);
    shell_input_reset(&input);
    for (unsigned i = 0; i < SHELL_LINE_MAX - 2; i++) type(&input, " ");
    type(&input, "ec");
    matches = (struct shell_matches){0};
    shell_match_add(&matches, "ec", "echo");
    CHECK(!shell_input_complete(&input, &matches));
    CHECK(input.length == SHELL_LINE_MAX && !input.overflow);
    type(&input, "\b\b\b\b");
    CHECK(shell_input_complete(&input, &matches));
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
            const char *prefix = shell_input_prefix(&input);
            if (prefix) {
                struct shell_matches matches = {0};
                shell_match_add(&matches, prefix, "cd");
                shell_match_add(&matches, prefix, "exit");
                shell_input_complete(&input, &matches);
            }
        }
        CHECK(input.length <= SHELL_LINE_MAX && input.line[input.length] == '\0');
        CHECK(input.count <= SHELL_HISTORY_CAPACITY && input.age <= input.count);
        CHECK(input.next < SHELL_HISTORY_CAPACITY && input.escape_bytes <= SHELL_ESCAPE_MAX);
        for (unsigned slot = 0; slot < SHELL_HISTORY_CAPACITY; slot++) {
            CHECK(strnlen(input.history[slot], SHELL_LINE_MAX + 1) <= SHELL_LINE_MAX);
        }
    }
    return 0;
}

int main(void) {
    int line = history();
    if (!line) line = terminal();
    if (!line) line = completion();
    if (!line) line = bounds();
    if (line) dprintf(STDERR_FILENO, "shell input failed at line %d\n", line);
    return line != 0;
}
