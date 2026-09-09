#include <unistd.h>
#include <time.h>

unsigned int sleep(unsigned int seconds) {
    const struct timespec duration = {
        .tv_sec = (time_t)seconds,
        .tv_nsec = 0,
    };
    return nanosleep(&duration, NULL) == 0 ? 0 : seconds;
}

int usleep(useconds_t usec) {
    const struct timespec duration = {
        .tv_sec = (time_t)(usec / 1000000u),
        .tv_nsec = (long)(usec % 1000000u) * 1000L,
    };
    return nanosleep(&duration, NULL);
}
