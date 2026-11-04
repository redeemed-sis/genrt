#pragma once

#include <string.h>
#include "shell_input.h"

#define SHELL_ARG_MAX 16

struct shell_command {
    char words[SHELL_LINE_MAX + 1];
    char *argv[SHELL_ARG_MAX + 1];
    const char *output;
    int append;
    int argc;
};

enum shell_parse_result {
    SHELL_PARSE_OK,
    SHELL_PARSE_TOO_MANY_ARGS,
    SHELL_PARSE_SYNTAX,
};

/* One stdout destination, with optional adjacent > or >>. The original line
 * stays intact for history. Unsupported shell operators are rejected before
 * fork/open, so an invalid command cannot truncate a destination. */
static enum shell_parse_result shell_parse(const char *line, struct shell_command *command) {
    *command = (struct shell_command){0};
    const char *source = line;
    char *out = command->words;
    int destination = 0;
    while (*source) {
        while (*source == ' ' || *source == '\t') source++;
        if (!*source) break;
        if (*source == '>') {
            if (destination || command->output || !command->argc) return SHELL_PARSE_SYNTAX;
            source++;
            if (*source == '>') { command->append = 1; source++; }
            destination = 1;
            continue;
        }
        char *word = out;
        int digits = 1;
        while (*source && *source != ' ' && *source != '\t' && *source != '>') {
            if (strchr("<|&;\"'\\", *source)) return SHELL_PARSE_SYNTAX;
            if (*source < '0' || *source > '9') digits = 0;
            if ((size_t)(out - command->words) >= SHELL_LINE_MAX) return SHELL_PARSE_SYNTAX;
            *out++ = *source++;
        }
        if (digits && *source == '>') return SHELL_PARSE_SYNTAX; // No fd redirects.
        if ((size_t)(out - command->words) > SHELL_LINE_MAX) return SHELL_PARSE_SYNTAX;
        *out++ = '\0';
        if (destination) {
            command->output = word;
            destination = 0;
        } else {
            if (command->argc == SHELL_ARG_MAX) return SHELL_PARSE_TOO_MANY_ARGS;
            command->argv[command->argc++] = word;
        }
    }
    return destination ? SHELL_PARSE_SYNTAX : SHELL_PARSE_OK;
}
