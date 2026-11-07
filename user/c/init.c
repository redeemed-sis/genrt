#include <stdio.h>
#include <syscall.h>
#include <sys/mount.h>

#include "init_fstab.h"

static char config[INIT_CONFIG_MAX + 1];
static struct init_mount entries[INIT_ENTRIES_MAX];

int main(void) {
    unsigned count = 0, line = 0;
    size_t length = 0;
    int fd = open("/etc/fstab", O_RDONLY);
    if (fd < 0 && fd != -GENRT_ENOENT) {
        dprintf(2, "init: open /etc/fstab failed (%d)\n", fd);
        return 1;
    }
    if (fd >= 0) {
        int error = 0;
        for (;;) {
            char extra;
            size_t available = INIT_CONFIG_MAX - length;
            ssize_t done = read(fd, available ? config + length : &extra,
                                available ? available : 1);
            if (done < 0) { error = (int)done; break; }
            if (done == 0) break;
            if (!available || (size_t)done > available) { error = -GENRT_EOVERFLOW; break; }
            length += (size_t)done;
        }
        int closed = close(fd);
        if (!error && closed < 0) error = closed;
        if (!error) error = init_fstab(config, length, entries, &count, &line);
        if (error) {
            dprintf(2, "init: /etc/fstab:%u invalid or unreadable (%d)\n", line, error);
            return 1;
        }
    }
    for (unsigned i = 0; i < count; i++) {
        const struct init_mount *entry = &entries[i];
        if (entry->noauto) continue;
        int result = mount(entry->source, entry->target, entry->type, entry->flags, NULL);
        if (result < 0) {
            dprintf(2, "init: warning: /etc/fstab:%u mount %s on %s failed (%d)\n",
                    entry->line, entry->source, entry->target, result);
        }
    }
    char *argv[] = {"shell", NULL};
    int result = execve("/bin/shell", argv, NULL);
    dprintf(2, "init: exec /bin/shell failed (%d)\n", result);
    return 1;
}
