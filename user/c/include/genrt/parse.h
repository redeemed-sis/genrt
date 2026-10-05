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

/* Same decimal grammar, checked inclusive bound, errors, and pointer contract
 * as genrt_parse_u32_decimal, with a u64 result. Failure leaves value unchanged. */
int genrt_parse_u64_decimal(const char *text, uint64_t max_value,
                            uint64_t *value);
