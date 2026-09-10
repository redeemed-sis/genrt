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

/* Shell-private, fixed storage. History and the saved draft never alias line.
 * age == 0 selects the draft; age == 1 selects the newest ring entry. */
struct shell_input {
    char line[SHELL_LINE_MAX + 1];
    char history[SHELL_HISTORY_CAPACITY][SHELL_LINE_MAX + 1];
    char draft[SHELL_LINE_MAX + 1];
    size_t length;
    size_t count;
    size_t next;
    size_t age;
    unsigned escape_bytes;
    enum shell_escape_state escape;
    int overflow;
    int draft_overflow;
    int skip_lf;
};

/* Start another line without losing history or the CRLF suppression state. */
static inline void shell_input_reset(struct shell_input *input) {
    input->line[0] = '\0';
    input->length = 0;
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
 * exact draft and its overflow flag captured by the first Up on this line. */
static inline enum shell_input_action shell_input_history(struct shell_input *input, int up) {
    if (up) {
        if (input->age == input->count) return SHELL_INPUT_NONE;
        if (input->age == 0) {
            memcpy(input->draft, input->line, input->length + 1);
            input->draft_overflow = input->overflow;
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
    memcpy(input->line, line, input->length + 1);
    input->overflow = input->age == 0 ? input->draft_overflow : 0;
    return SHELL_INPUT_REDRAW;
}

/* Feed one terminal byte. CSI/SS3 Up/Down are recognized only without
 * parameters. Other CSI sequences are discarded through their final byte,
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
        input->escape = SHELL_ESCAPE_NONE;
        if (input->length == 0) return SHELL_INPUT_NONE;
        input->line[--input->length] = '\0';
        return SHELL_INPUT_ERASE;
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
            input->escape = SHELL_ESCAPE_NONE;
            if (input->escape_bytes == 0 && (ch == 'A' || ch == 'B')) {
                return shell_input_history(input, ch == 'A');
            }
        } else if (input->escape_bytes < SHELL_ESCAPE_MAX) {
            input->escape_bytes++;
        }
        return SHELL_INPUT_NONE;
    }
    if (ch < ' ') return SHELL_INPUT_NONE;
    if (input->length == SHELL_LINE_MAX) {
        input->overflow = 1;
        return SHELL_INPUT_NONE;
    }
    input->line[input->length++] = (char)ch;
    input->line[input->length] = '\0';
    return SHELL_INPUT_APPEND;
}

/* Return the start of a bare first token, or NULL once arguments/path syntax
 * begin. The editor is append/backspace-only, so the cursor is always at end. */
static inline const char *shell_input_prefix(const struct shell_input *input) {
    if (input->overflow) return NULL;
    const char *prefix = input->line;
    while (*prefix == ' ') prefix++;
    for (const char *p = prefix; *p; p++) {
        if (*p == ' ' || *p == '\t' || *p == '/') return NULL;
    }
    return prefix;
}

struct shell_matches {
    char common[SHELL_LINE_MAX + 1];
    size_t length;
    unsigned count; /* Saturates at two: only none/one/many is needed. */
};

/* Accumulate a matching name without storing a candidate array. Names must
 * fit a shell line; the directory caller also enforces executable path bounds. */
static inline int shell_match_add(struct shell_matches *matches, const char *prefix,
                                 const char *name) {
    size_t length = strnlen(name, SHELL_LINE_MAX + 1);
    if (length == 0 || length > SHELL_LINE_MAX
        || strncmp(name, prefix, strlen(prefix)) != 0) return 0;
    for (size_t i = 0; i < length; i++) {
        if ((unsigned char)name[i] <= ' ' || (unsigned char)name[i] > '~'
            || name[i] == '/') return 0;
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

/* Complete within the existing line bound. Return true when text changes.
 * A unique match gets a trailing space when there is room for it. */
static inline int shell_input_complete(struct shell_input *input,
                                       const struct shell_matches *matches) {
    const char *prefix = shell_input_prefix(input);
    if (prefix == NULL || matches->count == 0) return 0;
    size_t start = (size_t)(prefix - input->line);
    if (matches->length > SHELL_LINE_MAX - start) return 0;
    size_t old_length = input->length;
    if (matches->length <= strlen(prefix) && matches->count != 1) return 0;
    memcpy(input->line + start, matches->common, matches->length);
    input->length = start + matches->length;
    if (matches->count == 1 && input->length < SHELL_LINE_MAX) {
        input->line[input->length++] = ' ';
    }
    input->line[input->length] = '\0';
    return input->length != old_length;
}
