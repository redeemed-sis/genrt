#pragma once

#include <stdint.h>

#include <genrt/errno.h>

/*
 * Parse one nonempty ASCII decimal u32. On success stores a value no greater
 * than max_value and returns zero. Invalid input returns -GENRT_EINVAL;
 * arithmetic or configured-limit overflow returns -GENRT_ERANGE. text and
 * value must be non-null, valid pointers.
 */
int genrt_parse_u32_decimal(const char *text, uint32_t max_value,
                            uint32_t *value);
