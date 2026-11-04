#pragma once

/* Positive errno values used by freestanding userspace support APIs. */
#define GENRT_EIO 5
#define GENRT_EPERM 1
#define GENRT_ENOENT 2
#define GENRT_EBADF 9
#define GENRT_ENOMEM 12
#define GENRT_EFAULT 14
#define GENRT_EBUSY 16
#define GENRT_EEXIST 17
#define GENRT_ENOTDIR 20
#define GENRT_EINVAL 22
#define GENRT_EMFILE 24
#define GENRT_ENOSPC 28
#define GENRT_EROFS 30
#define GENRT_ENOSYS 38
#define GENRT_ERANGE 34
#define GENRT_EOVERFLOW 75
#define GENRT_ENOTSUP 95
#define GENRT_ETIMEDOUT 110
