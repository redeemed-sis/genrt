#pragma once

#include <stddef.h>
#include <string.h>

#define SHELL_LINE_MAX 256
#define SHELL_HISTORY_CAPACITY 16
#define SHELL_ESCAPE_MAX 16

enum shell_input_action {
    SHELL_INPUT_NONE,
    SHELL_INPUT_APPEND,
    SHELL_INPUT_ERASE,
    SHELL_INPUT_REDRAW,
    SHELL_INPUT_COMPLETE,
    SHELL_INPUT_GEOMETRY,
    SHELL_INPUT_SUBMIT,
};

enum shell_escape_state {
    SHELL_ESCAPE_NONE,
    SHELL_ESCAPE_START,
    SHELL_ESCAPE_INTERMEDIATE,
    SHELL_ESCAPE_CSI,
    SHELL_ESCAPE_SS3,
    SHELL_ESCAPE_STRING,
    SHELL_ESCAPE_STRING_END,
};

/* Shell-private, fixed storage; 0 <= cursor <= length <= SHELL_LINE_MAX.
 * History and the saved draft never alias line.
 * age == 0 selects the draft; age == 1 selects the newest ring entry. */
struct shell_input {
    char line[SHELL_LINE_MAX + 1];
    char history[SHELL_HISTORY_CAPACITY][SHELL_LINE_MAX + 1];
    char draft[SHELL_LINE_MAX + 1];
    size_t length;
    size_t cursor;
    size_t draft_cursor;
    size_t count;
    size_t next;
    size_t age;
    unsigned escape_bytes;
    unsigned escape_parameters[2];
    unsigned escape_parameter;
    int escape_parameters_valid;
    unsigned terminal_rows, terminal_columns;
    enum shell_escape_state escape;
    int overflow;
    int draft_overflow;
    int skip_lf;
};

/* Start another line without losing history or the CRLF suppression state. */
static inline void shell_input_reset(struct shell_input *input) {
    input->line[0] = '\0';
    input->length = 0;
    input->cursor = 0;
    input->age = 0;
    input->overflow = 0;
    input->escape = SHELL_ESCAPE_NONE;
    input->escape_bytes = 0;
}

/* Remember an accepted raw line before execution tokenizes it in place.
 * Empty/whitespace-only, overlong, and consecutive duplicate lines are skipped. */
static inline void shell_input_remember(struct shell_input *input) {
    if (input->overflow) return;
    size_t first = 0;
    while (input->line[first] == ' ' || input->line[first] == '\t') first++;
    if (input->line[first] == '\0') return;
    size_t newest = (input->next + SHELL_HISTORY_CAPACITY - 1) % SHELL_HISTORY_CAPACITY;
    if (input->count != 0 && strcmp(input->history[newest], input->line) == 0) return;
    memcpy(input->history[input->next], input->line, input->length + 1);
    input->next = (input->next + 1) % SHELL_HISTORY_CAPACITY;
    if (input->count < SHELL_HISTORY_CAPACITY) input->count++;
}

/* Navigate without changing saved commands. Down past newest restores the
 * exact draft, cursor and overflow flag captured by the first Up on this line. */
static inline enum shell_input_action shell_input_history(struct shell_input *input, int up) {
    if (up) {
        if (input->age == input->count) return SHELL_INPUT_NONE;
        if (input->age == 0) {
            memcpy(input->draft, input->line, input->length + 1);
            input->draft_overflow = input->overflow;
            input->draft_cursor = input->cursor;
        }
        input->age++;
    } else {
        if (input->age == 0) return SHELL_INPUT_NONE;
        input->age--;
    }
    const char *line = input->age == 0 ? input->draft
        : input->history[(input->next + SHELL_HISTORY_CAPACITY - input->age)
                         % SHELL_HISTORY_CAPACITY];
    input->length = strlen(line);
    input->cursor = input->age == 0 ? input->draft_cursor : input->length;
    memcpy(input->line, line, input->length + 1);
    input->overflow = input->age == 0 ? input->draft_overflow : 0;
    return SHELL_INPUT_REDRAW;
}

static inline int shell_word_delimiter(char ch) {
    return ch == ' ' || ch == '\t' || ch == '/' || ch == '>';
}

/* Delete to the left, never touching the suffix or clearing overflow. A word
 * deletion with only preceding delimiters is deliberately a no-op. */
static inline enum shell_input_action shell_input_backspace(struct shell_input *input,
                                                            int word) {
    size_t start = input->cursor;
    if (word) {
        while (start && shell_word_delimiter(input->line[start - 1])) start--;
        if (!start) return SHELL_INPUT_NONE;
        while (start && !shell_word_delimiter(input->line[start - 1])) start--;
    } else {
        if (!start) return SHELL_INPUT_NONE;
        start--;
    }
    size_t removed = input->cursor - start;
    memmove(input->line + start, input->line + input->cursor,
            input->length - input->cursor + 1);
    input->length -= removed;
    input->cursor = start;
    return SHELL_INPUT_ERASE;
}

/* Feed one terminal byte. CSI/SS3 arrows are recognized only without
 * parameters; bounded CSI row;column R reports produce a geometry action.
 * Other CSI sequences are discarded through their final byte,
 * with a saturating counter; OSC/DCS strings are discarded until BEL or ST.
 * Newline, backspace, and Tab resynchronize incomplete sequences. A new ESC
 * restarts a sequence outside string controls. No control payload is copied
 * into the command. */
static inline enum shell_input_action shell_input_feed(struct shell_input *input,
                                                       unsigned char ch) {
    if (input->skip_lf && ch == '\n') {
        input->skip_lf = 0;
        return SHELL_INPUT_NONE;
    }
    input->skip_lf = 0;
    if (ch == '\r' || ch == '\n') {
        input->skip_lf = ch == '\r';
        input->escape = SHELL_ESCAPE_NONE;
        return SHELL_INPUT_SUBMIT;
    }
    if (ch == '\b' || ch == 0x7f) {
        int word = input->escape == SHELL_ESCAPE_START;
        input->escape = SHELL_ESCAPE_NONE;
        return shell_input_backspace(input, word);
    }
    if (ch == '\t') {
        input->escape = SHELL_ESCAPE_NONE;
        return SHELL_INPUT_COMPLETE;
    }
    if (input->escape == SHELL_ESCAPE_STRING || input->escape == SHELL_ESCAPE_STRING_END) {
        if (ch == '\a' || (input->escape == SHELL_ESCAPE_STRING_END && ch == '\\')) {
            input->escape = SHELL_ESCAPE_NONE;
        } else {
            input->escape = ch == '\033' ? SHELL_ESCAPE_STRING_END : SHELL_ESCAPE_STRING;
        }
        return SHELL_INPUT_NONE;
    }
    if (ch == '\033') {
        input->escape = SHELL_ESCAPE_START;
        input->escape_bytes = 0;
        input->escape_parameters[0] = input->escape_parameters[1] = 0;
        input->escape_parameter = 0;
        input->escape_parameters_valid = 1;
        return SHELL_INPUT_NONE;
    }
    if (input->escape == SHELL_ESCAPE_START) {
        input->escape = ch == '[' ? SHELL_ESCAPE_CSI
            : ch == 'O' ? SHELL_ESCAPE_SS3
            : (ch == ']' || ch == 'P' || ch == 'X' || ch == '^' || ch == '_')
                ? SHELL_ESCAPE_STRING
            : (ch >= 0x20 && ch <= 0x2f) ? SHELL_ESCAPE_INTERMEDIATE : SHELL_ESCAPE_NONE;
        return SHELL_INPUT_NONE;
    }
    if (input->escape == SHELL_ESCAPE_INTERMEDIATE) {
        // For example ESC ( B selects a character set; its final byte is not
        // command input even though this editor does not implement the control.
        if (ch < 0x20 || ch > 0x2f) input->escape = SHELL_ESCAPE_NONE;
        return SHELL_INPUT_NONE;
    }
    if (input->escape != SHELL_ESCAPE_NONE) {
        if (ch >= 0x40 && ch <= 0x7e) {
            int csi = input->escape == SHELL_ESCAPE_CSI;
            input->escape = SHELL_ESCAPE_NONE;
            if (csi && ch == 'R' && input->escape_parameters_valid
                && input->escape_parameter == 1 && input->escape_parameters[0]
                && input->escape_parameters[1]) {
                input->terminal_rows = input->escape_parameters[0];
                input->terminal_columns = input->escape_parameters[1];
                return SHELL_INPUT_GEOMETRY;
            }
            if (input->escape_bytes == 0 && (ch == 'A' || ch == 'B')) {
                return shell_input_history(input, ch == 'A');
            }
            if (input->escape_bytes == 0 && ch == 'D' && input->cursor) {
                input->cursor--;
                return SHELL_INPUT_REDRAW;
            }
            if (input->escape_bytes == 0 && ch == 'C' && input->cursor < input->length) {
                input->cursor++;
                return SHELL_INPUT_REDRAW;
            }
        } else {
            if (input->escape_bytes < SHELL_ESCAPE_MAX) input->escape_bytes++;
            unsigned *value = &input->escape_parameters[input->escape_parameter];
            if (input->escape == SHELL_ESCAPE_CSI && ch >= '0' && ch <= '9'
                && *value < 1000) {
                *value = *value * 10 + (unsigned)(ch - '0');
            } else if (input->escape == SHELL_ESCAPE_CSI && ch == ';'
                       && input->escape_parameter == 0) {
                input->escape_parameter = 1;
            } else input->escape_parameters_valid = 0;
        }
        return SHELL_INPUT_NONE;
    }
    if (ch < ' ') return SHELL_INPUT_NONE;
    if (input->length == SHELL_LINE_MAX) {
        input->overflow = 1;
        return SHELL_INPUT_NONE;
    }
    int append = input->cursor == input->length;
    memmove(input->line + input->cursor + 1, input->line + input->cursor,
            input->length - input->cursor + 1);
    input->line[input->cursor++] = (char)ch;
    input->length++;
    return append ? SHELL_INPUT_APPEND : SHELL_INPUT_REDRAW;
}

struct shell_completion {
    size_t start;
    size_t end;
    size_t token_end;
    int commands;
    char prefix[SHELL_LINE_MAX + 1];
    char directory[SHELL_LINE_MAX + 1];
};

static inline int shell_token_delimiter(char ch) {
    return ch == ' ' || ch == '\t' || ch == '>';
}

static inline int shell_name_byte(unsigned char ch) {
    return ch > ' ' && ch <= '~' && strchr("/<|&;\"'\\>", ch) == NULL;
}

/* Describe the component under the cursor without modifying the line. The
 * directory spelling (including ./ and ../) is passed to open unchanged. */
static inline int shell_input_completion(const struct shell_input *input,
                                         struct shell_completion *completion) {
    if (input->overflow) return 0;
    size_t token = input->cursor;
    while (token && !shell_token_delimiter(input->line[token - 1])) token--;
    size_t end = input->cursor;
    while (end < input->length && !shell_token_delimiter(input->line[end])) end++;
    size_t first = 0;
    while (first < input->length && (input->line[first] == ' ' || input->line[first] == '\t')) first++;
    size_t component = token;
    int slash = 0;
    for (size_t i = token; i < end; i++) {
        if (input->line[i] == '/') {
            slash = 1;
            if (i < input->cursor) component = i + 1;
        } else if (!shell_name_byte((unsigned char)input->line[i])) return 0;
    }
    completion->commands = token == first && !slash;
    completion->start = component;
    completion->token_end = end;
    completion->end = input->cursor;
    while (completion->end < end && input->line[completion->end] != '/') completion->end++;
    size_t length = input->cursor - component;
    memcpy(completion->prefix, input->line + component, length);
    completion->prefix[length] = '\0';
    length = component - token;
    if (length) {
        memcpy(completion->directory, input->line + token, length);
        completion->directory[length] = '\0';
    } else {
        memcpy(completion->directory, ".", 2);
    }
    return 1;
}

struct shell_matches {
    char common[SHELL_LINE_MAX + 1];
    size_t length;
    unsigned count; /* Saturates at two: only none/one/many is needed. */
    int directory; /* Kind of the unique match, ignored for multiple matches. */
};

/* Accumulate a matching name without storing a candidate array. Names must
 * fit a shell line; the directory caller also enforces executable path bounds. */
static inline int shell_match_add(struct shell_matches *matches, const char *prefix,
                                 const char *name) {
    size_t length = strnlen(name, SHELL_LINE_MAX + 1);
    if (length == 0 || length > SHELL_LINE_MAX
        || strncmp(name, prefix, strlen(prefix)) != 0) return 0;
    for (size_t i = 0; i < length; i++) {
        if (!shell_name_byte((unsigned char)name[i])) return 0;
    }
    if (matches->count == 0) {
        memcpy(matches->common, name, length + 1);
        matches->length = length;
    } else {
        size_t common = 0;
        while (common < matches->length && common < length
               && matches->common[common] == name[common]) common++;
        matches->length = common;
        matches->common[common] = '\0';
    }
    if (matches->count < 2) matches->count++;
    return 1;
}

/* Existing component text to the right must match literally and survive.
 * Accumulate only the part before it, so LCP extension inserts at cursor. */
static inline int shell_completion_add(struct shell_matches *matches,
                                       const struct shell_input *input,
                                       const struct shell_completion *completion,
                                       const char *name, int directory) {
    size_t length = strnlen(name, SHELL_LINE_MAX + 1);
    size_t suffix = completion->end - input->cursor;
    size_t prefix = input->cursor - completion->start;
    if (length > SHELL_LINE_MAX || length < prefix + suffix
        || (completion->end < completion->token_end && !directory)
        || memcmp(name + length - suffix, input->line + input->cursor, suffix) != 0) return 0;
    for (size_t i = 0; i < length; i++) {
        if (!shell_name_byte((unsigned char)name[i])) return 0;
    }
    char before_suffix[SHELL_LINE_MAX + 1];
    memcpy(before_suffix, name, length - suffix);
    before_suffix[length - suffix] = '\0';
    /* An empty prefix before an existing suffix is still a valid match. */
    if (length == suffix) {
        matches->common[0] = '\0';
        matches->length = 0;
        if (matches->count < 2) matches->count++;
    } else if (!shell_match_add(matches, completion->prefix, before_suffix)) return 0;
    matches->directory = directory;
    return 1;
}

/* Insert only the extension at cursor. Delimiters are added only at token end,
 * never before an existing component suffix, slash, argument, or operator. */
static inline int shell_input_complete(struct shell_input *input,
                                       const struct shell_completion *completion,
                                       const struct shell_matches *matches) {
    if (input->overflow || matches->count == 0) return 0;
    size_t prefix = input->cursor - completion->start;
    if (matches->length < prefix) return 0;
    size_t added = matches->length - prefix;
    if (added > SHELL_LINE_MAX - input->length) return 0;
    char delimiter = '\0';
    if (matches->count == 1 && input->cursor == completion->token_end
        && input->length + added < SHELL_LINE_MAX) {
        if (matches->directory) delimiter = '/';
        else if (input->cursor == input->length) delimiter = ' ';
    }
    size_t total = added + (delimiter != '\0');
    if (!total) return 0;
    memmove(input->line + input->cursor + total, input->line + input->cursor,
            input->length - input->cursor + 1);
    memcpy(input->line + input->cursor, matches->common + prefix, added);
    if (delimiter) input->line[input->cursor + added] = delimiter;
    input->length += total;
    input->cursor += total;
    return 1;
}
