#include <stdio.h>

#include <limits.h>
#include <stdint.h>
#include <string.h>

#include <unistd.h>

#define GENRT_STDIO_WRITE_CHUNK 256

enum length_modifier {
    LENGTH_DEFAULT,
    LENGTH_LONG,
    LENGTH_LONG_LONG,
    LENGTH_SIZE,
};

enum output_kind {
    OUTPUT_MEMORY,
    OUTPUT_FD,
};

struct output {
    enum output_kind kind;
    char *buffer;
    size_t size;
    size_t stored;
    int fd;
    char chunk[GENRT_STDIO_WRITE_CHUNK];
    size_t chunk_used;
    int count;
    int error;
};

static int flush_chunk(struct output *out) {
    size_t written = 0;
    while (written < out->chunk_used) {
        const ssize_t result = write(
            out->fd, out->chunk + written, out->chunk_used - written);
        if (result < 0) {
            return (int)result;
        }
        if (result == 0 || (size_t)result > out->chunk_used - written) {
            return -GENRT_EIO;
        }
        written += (size_t)result;
    }
    out->chunk_used = 0;
    return 0;
}

static void output_count(struct output *out, size_t count) {
    if (out->error != 0) {
        return;
    }
    if (count > (size_t)INT_MAX - (size_t)out->count) {
        out->error = -GENRT_EOVERFLOW;
        return;
    }
    out->count += (int)count;
}

static void output_character(struct output *out, char character) {
    output_count(out, 1);
    if (out->error != 0) {
        return;
    }
    if (out->kind == OUTPUT_MEMORY) {
        if (out->stored + 1 < out->size) {
            out->buffer[out->stored++] = character;
        }
        return;
    }
    out->chunk[out->chunk_used++] = character;
    if (out->chunk_used == sizeof(out->chunk)) {
        out->error = flush_chunk(out);
    }
}

static void output_repeat(struct output *out, char character, size_t count) {
    output_count(out, count);
    if (out->error != 0) {
        return;
    }
    if (out->kind == OUTPUT_MEMORY) {
        size_t available = 0;
        if (out->stored + 1 < out->size) {
            available = out->size - out->stored - 1;
        }
        if (available > count) {
            available = count;
        }
        if (available != 0) {
            memset(out->buffer + out->stored, character, available);
            out->stored += available;
        }
        return;
    }
    while (count-- != 0 && out->error == 0) {
        out->chunk[out->chunk_used++] = character;
        if (out->chunk_used == sizeof(out->chunk)) {
            out->error = flush_chunk(out);
        }
    }
}

static void output_text(struct output *out, const char *text) {
    const size_t length = strlen(text);
    output_count(out, length);
    if (out->error != 0) {
        return;
    }
    if (out->kind == OUTPUT_MEMORY) {
        size_t available = 0;
        if (out->stored + 1 < out->size) {
            available = out->size - out->stored - 1;
        }
        if (available > length) {
            available = length;
        }
        if (available != 0) {
            memcpy(out->buffer + out->stored, text, available);
            out->stored += available;
        }
        return;
    }
    for (size_t index = 0; index < length && out->error == 0; index++) {
        out->chunk[out->chunk_used++] = text[index];
        if (out->chunk_used == sizeof(out->chunk)) {
            out->error = flush_chunk(out);
        }
    }
}

static int parse_width(const char **format, size_t *width) {
    size_t value = 0;
    while (**format >= '0' && **format <= '9') {
        const size_t digit = (size_t)(**format - '0');
        if (value > ((size_t)INT_MAX - digit) / 10) {
            return -GENRT_EOVERFLOW;
        }
        value = value * 10 + digit;
        (*format)++;
    }
    *width = value;
    return 0;
}

static size_t unsigned_digits(uintmax_t value, unsigned int base) {
    size_t digits = 1;
    while (value >= base) {
        value /= base;
        digits++;
    }
    return digits;
}

static void output_unsigned(struct output *out, uintmax_t value,
                            unsigned int base, int uppercase, size_t width,
                            int zero_pad, char sign) {
    static const char lower[] = "0123456789abcdef";
    static const char upper[] = "0123456789ABCDEF";
    const char *digits = uppercase ? upper : lower;
    const size_t digit_count = unsigned_digits(value, base);
    const size_t content_count = digit_count + (sign == '\0' ? 0 : 1);
    const size_t padding = width > content_count ? width - content_count : 0;

    if (!zero_pad) {
        output_repeat(out, ' ', padding);
    }
    if (sign != '\0') {
        output_character(out, sign);
    }
    if (zero_pad) {
        output_repeat(out, '0', padding);
    }
    char reversed[sizeof(uintmax_t) * CHAR_BIT];
    for (size_t index = 0; index < digit_count; index++) {
        reversed[index] = digits[value % base];
        value /= base;
    }
    for (size_t index = digit_count; index != 0; index--) {
        output_character(out, reversed[index - 1]);
    }
}

static void output_signed(struct output *out, intmax_t value, size_t width,
                          int zero_pad) {
    const int negative = value < 0;
    const uintmax_t magnitude = negative ? 0 - (uintmax_t)value : (uintmax_t)value;
    output_unsigned(out, magnitude, 10, 0, width, zero_pad, negative ? '-' : '\0');
}

static int format_output(struct output *out, const char *format,
                         va_list arguments) {
    if (format == NULL) {
        return -GENRT_EINVAL;
    }
    for (const char *cursor = format; out->error == 0 && *cursor != '\0'; cursor++) {
        if (*cursor != '%') {
            output_character(out, *cursor);
            continue;
        }

        cursor++;
        if (*cursor == '\0') {
            out->error = -GENRT_EINVAL;
            break;
        }
        int zero_pad = 0;
        if (*cursor == '0') {
            zero_pad = 1;
            cursor++;
        }
        size_t width = 0;
        const int width_result = parse_width(&cursor, &width);
        if (width_result != 0) {
            out->error = width_result;
            break;
        }
        enum length_modifier length = LENGTH_DEFAULT;
        if (*cursor == 'l') {
            length = LENGTH_LONG;
            cursor++;
            if (*cursor == 'l') {
                length = LENGTH_LONG_LONG;
                cursor++;
            }
        } else if (*cursor == 'z') {
            length = LENGTH_SIZE;
            cursor++;
        }

        switch (*cursor) {
        case '%':
            if (length != LENGTH_DEFAULT || width != 0 || zero_pad) {
                out->error = -GENRT_EINVAL;
            } else {
                output_character(out, '%');
            }
            break;
        case 'c':
            if (length != LENGTH_DEFAULT) {
                out->error = -GENRT_EINVAL;
            } else {
                output_repeat(out, ' ', width > 1 ? width - 1 : 0);
                output_character(out, (char)va_arg(arguments, int));
            }
            break;
        case 's': {
            if (length != LENGTH_DEFAULT) {
                out->error = -GENRT_EINVAL;
                break;
            }
            const char *text = va_arg(arguments, const char *);
            if (text == NULL) {
                text = "(null)";
            }
            const size_t text_length = strlen(text);
            output_repeat(out, ' ', width > text_length ? width - text_length : 0);
            output_text(out, text);
            break;
        }
        case 'd':
        case 'i':
            switch (length) {
            case LENGTH_LONG:
                output_signed(out, va_arg(arguments, long), width, zero_pad);
                break;
            case LENGTH_LONG_LONG:
                output_signed(out, va_arg(arguments, long long), width, zero_pad);
                break;
            case LENGTH_SIZE:
                output_signed(out, va_arg(arguments, ptrdiff_t), width, zero_pad);
                break;
            case LENGTH_DEFAULT:
                output_signed(out, va_arg(arguments, int), width, zero_pad);
                break;
            }
            break;
        case 'u':
            switch (length) {
            case LENGTH_LONG:
                output_unsigned(out, va_arg(arguments, unsigned long), 10, 0, width,
                                zero_pad, '\0');
                break;
            case LENGTH_LONG_LONG:
                output_unsigned(out, va_arg(arguments, unsigned long long), 10, 0,
                                width, zero_pad, '\0');
                break;
            case LENGTH_SIZE:
                output_unsigned(out, va_arg(arguments, size_t), 10, 0, width,
                                zero_pad, '\0');
                break;
            case LENGTH_DEFAULT:
                output_unsigned(out, va_arg(arguments, unsigned int), 10, 0, width,
                                zero_pad, '\0');
                break;
            }
            break;
        case 'x':
        case 'X':
            switch (length) {
            case LENGTH_LONG:
                output_unsigned(out, va_arg(arguments, unsigned long), 16,
                                *cursor == 'X', width, zero_pad, '\0');
                break;
            case LENGTH_LONG_LONG:
                output_unsigned(out, va_arg(arguments, unsigned long long), 16,
                                *cursor == 'X', width, zero_pad, '\0');
                break;
            case LENGTH_SIZE:
                output_unsigned(out, va_arg(arguments, size_t), 16, *cursor == 'X',
                                width, zero_pad, '\0');
                break;
            case LENGTH_DEFAULT:
                output_unsigned(out, va_arg(arguments, unsigned int), 16,
                                *cursor == 'X', width, zero_pad, '\0');
                break;
            }
            break;
        case 'p':
            if (length != LENGTH_DEFAULT) {
                out->error = -GENRT_EINVAL;
            } else {
                const uintptr_t pointer = (uintptr_t)va_arg(arguments, void *);
                const size_t digits = unsigned_digits(pointer, 16);
                if (!zero_pad) {
                    output_repeat(out, ' ', width > digits + 2 ? width - digits - 2 : 0);
                }
                output_text(out, "0x");
                output_unsigned(out, pointer, 16, 0,
                                zero_pad && width > 2 ? width - 2 : 0, zero_pad, '\0');
            }
            break;
        default:
            out->error = -GENRT_EINVAL;
            break;
        }
    }
    return out->error;
}

int vsnprintf(char *buffer, size_t size, const char *format, va_list arguments) {
    if (buffer == NULL && size != 0) {
        return -GENRT_EINVAL;
    }
    struct output out = {
        .kind = OUTPUT_MEMORY,
        .buffer = buffer,
        .size = size,
        .stored = 0,
        .fd = -1,
        .chunk_used = 0,
        .count = 0,
        .error = 0,
    };
    const int error = format_output(&out, format, arguments);
    if (size != 0) {
        buffer[out.stored] = '\0';
    }
    return error != 0 ? error : out.count;
}

int snprintf(char *buffer, size_t size, const char *format, ...) {
    va_list arguments;
    va_start(arguments, format);
    const int result = vsnprintf(buffer, size, format, arguments);
    va_end(arguments);
    return result;
}

int vdprintf(int fd, const char *format, va_list arguments) {
    struct output out = {
        .kind = OUTPUT_FD,
        .buffer = NULL,
        .size = 0,
        .stored = 0,
        .fd = fd,
        .chunk_used = 0,
        .count = 0,
        .error = 0,
    };
    const int error = format_output(&out, format, arguments);
    if (error != 0) {
        return error;
    }
    const int flush_error = flush_chunk(&out);
    return flush_error != 0 ? flush_error : out.count;
}

int dprintf(int fd, const char *format, ...) {
    va_list arguments;
    va_start(arguments, format);
    const int result = vdprintf(fd, format, arguments);
    va_end(arguments);
    return result;
}

int vprintf(const char *format, va_list arguments) {
    return vdprintf(STDOUT_FILENO, format, arguments);
}

int printf(const char *format, ...) {
    va_list arguments;
    va_start(arguments, format);
    const int result = vdprintf(STDOUT_FILENO, format, arguments);
    va_end(arguments);
    return result;
}
