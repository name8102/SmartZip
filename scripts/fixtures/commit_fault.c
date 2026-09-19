/* Linux release-test interposer: pause at a real rename, without product hooks.
 * Only the temporary target named by SZ_FAULT_TARGET is intercepted.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void pause_commit(void) {
    const char *marker = getenv("SZ_FAULT_MARKER");
    if (!marker) return;
    int fd = open(marker, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) return;
    write(fd, "ready", 5);
    close(fd);
    raise(SIGSTOP);
}

int renameat2(int oldfd, const char *old, int newfd, const char *new, unsigned flags) {
    int (*real_rename)(int, const char *, int, const char *, unsigned) = dlsym(RTLD_NEXT, "renameat2");
    const char *target = getenv("SZ_FAULT_TARGET");
    const char *phase = getenv("SZ_FAULT_PHASE");
    if (target && phase && !strcmp(phase, "before-publish") && !strcmp(new, target)) pause_commit();
    int result = real_rename(oldfd, old, newfd, new, flags);
    if (result == 0 && target && phase) {
        if (!strcmp(phase, "after-backup") && !strcmp(old, target)) pause_commit();
        if (!strcmp(phase, "after-publish") && !strcmp(new, target)) pause_commit();
    }
    return result;
}
