#pragma once

#include <genrt/errno.h>
#include <string.h>
#include <sys/mount.h>

#define INIT_CONFIG_MAX 16384u
#define INIT_ENTRIES_MAX 16u

struct init_mount {
    char *source, *target, *type;
    unsigned long flags;
    unsigned line, disk, partition;
    int noauto;
};

static int init_space(unsigned char c) {
    return c == ' ' || c == '\t' || c == '\r' || c == '\v' || c == '\f';
}

/* Split before decoding so escaped whitespace remains inside one field. */
static int init_field(char *field) {
    char *out = field;
    while (*field) {
        unsigned value = (unsigned char)*field++;
        if (value == '\\') {
            value = 0;
            for (unsigned i = 0; i < 3; i++) {
                if (*field < '0' || *field > '7') return -GENRT_EINVAL;
                value = value * 8 + (unsigned)(*field++ - '0');
            }
        }
        if (value == 0 || value > 255) return -GENRT_EINVAL;
        *out++ = (char)value;
    }
    *out = 0;
    return 0;
}

static int init_number(char **text, unsigned *number) {
    char *p = *text;
    unsigned value = 0;
    if (*p < '0' || *p > '9') return -GENRT_EINVAL;
    while (*p >= '0' && *p <= '9') {
        if (value > 1000) return -GENRT_EINVAL;
        value = value * 10 + (unsigned)(*p++ - '0');
    }
    *text = p;
    *number = value;
    return 0;
}

static int init_source(struct init_mount *entry) {
    if (strncmp(entry->source, "/dev/disk", 9) != 0) return -GENRT_EINVAL;
    char *p = entry->source + 9;
    if (init_number(&p, &entry->disk) || *p++ != 'p'
        || init_number(&p, &entry->partition) || *p
        || entry->disk >= 32 || entry->partition < 1 || entry->partition > 4)
        return -GENRT_EINVAL;
    return 0;
}

/* Parse the whole owned input before applying any mount. Fields point into
 * that input and remain valid until initialization finishes. */
static int init_fstab(char *text, size_t length, struct init_mount *entries,
                      unsigned *count, unsigned *error_line) {
    *count = 0;
    *error_line = 0;
    if (length > INIT_CONFIG_MAX) return -GENRT_EOVERFLOW;
    for (size_t i = 0; i < length; i++) if (text[i] == 0) return -GENRT_EINVAL;
    text[length] = 0;
    unsigned line = 0;
    char *cursor = text;
    while (*cursor) {
        char *next = strchr(cursor, '\n');
        if (next) *next++ = 0;
        line++;
        *error_line = line;
        char *fields[6];
        unsigned n = 0;
        while (*cursor) {
            while (init_space((unsigned char)*cursor)) cursor++;
            if (!*cursor || *cursor == '#') break;
            if (n == 6) return -GENRT_EINVAL;
            fields[n++] = cursor;
            while (*cursor && !init_space((unsigned char)*cursor)) cursor++;
            if (*cursor) *cursor++ = 0;
        }
        if (n) {
            if (n != 6) return -GENRT_EINVAL;
            if (*count == INIT_ENTRIES_MAX) return -GENRT_EOVERFLOW;
            for (unsigned i = 0; i < 6; i++) if (init_field(fields[i])) return -GENRT_EINVAL;
            struct init_mount *entry = &entries[*count];
            *entry = (struct init_mount){.source = fields[0], .target = fields[1],
                                        .type = fields[2], .line = line};
            if (init_source(entry) || entry->target[0] != '/'
                || strlen(entry->target) >= 4096) return -GENRT_EINVAL;
            if (strcmp(entry->type, "vfat") && strcmp(entry->type, "fat32")) return -GENRT_ENOTSUP;
            char *option = fields[3];
            for (;;) {
                char *comma = strchr(option, ',');
                if (comma) *comma++ = 0;
                if (strcmp(option, "ro") == 0) entry->flags = MS_RDONLY;
                else if (strcmp(option, "rw") == 0) entry->flags = 0;
                else if (strcmp(option, "noauto") == 0) entry->noauto = 1;
                else if (strcmp(option, "defaults")) return -GENRT_EINVAL;
                if (!comma) break;
                option = comma;
            }
            for (unsigned i = 4; i < 6; i++) {
                if (!*fields[i]) return -GENRT_EINVAL;
                for (char *p = fields[i]; *p; p++) if (*p != '0') return -GENRT_EINVAL;
            }
            for (unsigned i = 0; i < *count; i++) {
                if (entries[i].disk == entry->disk && entries[i].partition == entry->partition)
                    return -GENRT_EINVAL;
            }
            (*count)++;
        }
        if (!next) break;
        cursor = next;
    }
    return 0;
}
