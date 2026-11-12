#ifndef NXKIT_LAUNCH_LOG_H
#define NXKIT_LAUNCH_LOG_H
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

// Direct descriptor redirection also captures Rust eprintln! and synchronous
// fatal diagnostics. No pipe-draining thread can lose the last line on exit.
static inline int nxkit_open_launch_log(const char *path) {
    int saved_stderr = dup(STDERR_FILENO);
    if (saved_stderr < 0) return -1;
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) { int saved = errno; close(saved_stderr); errno = saved; return -1; }
    if (dup2(fd, STDERR_FILENO) < 0) {
        int saved = errno;
        if (fd != STDERR_FILENO) close(fd);
        close(saved_stderr);
        errno = saved;
        return -1;
    }
    if (fd != STDERR_FILENO) close(fd);
    setbuf(stderr, NULL);
    return saved_stderr;
}

static inline int nxkit_close_launch_log(int saved_stderr) {
    if (saved_stderr < 0) return -1;
    int result = dup2(saved_stderr, STDERR_FILENO);
    int saved = errno;
    close(saved_stderr);
    errno = saved;
    return result < 0 ? -1 : 0;
}

// Called only after capture stops or on the next launch, never while guest
// threads write the file. Keep the tail, where the final fatal diagnostic lives.
static inline int nxkit_trim_launch_log(const char *path, size_t limit) {
    if (limit == 0) { errno = EINVAL; return -1; }
    int fd = open(path, O_RDWR);
    if (fd < 0) return -1;
    struct stat info;
    int result = -1;
    char *tail = NULL;
    if (fstat(fd, &info) != 0) goto done;
    if (info.st_size <= (off_t)limit) { result = 0; goto done; }
    tail = (char *)malloc(limit);
    if (!tail || lseek(fd, info.st_size - (off_t)limit, SEEK_SET) < 0) goto done;
    size_t used = 0;
    while (used < limit) {
        ssize_t count = read(fd, tail + used, limit - used);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) goto done;
        used += (size_t)count;
    }
    if (lseek(fd, 0, SEEK_SET) < 0) goto done;
    used = 0;
    while (used < limit) {
        ssize_t count = write(fd, tail + used, limit - used);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) goto done;
        used += (size_t)count;
    }
    result = ftruncate(fd, (off_t)limit);
done:
    { int saved = errno; free(tail); close(fd); errno = saved; }
    return result;
}
#endif
