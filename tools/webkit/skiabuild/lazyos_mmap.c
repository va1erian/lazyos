/* A read-only private mapping of a file, for a kernel that has none.
 *
 * LazyOS has no file-backed mmap (docs/linux-abi-plan.md): mmap(fd, PROT_READ,
 * MAP_PRIVATE) of a regular file fails with ENODEV (kernel/src/process/linux/
 * mmapfile.rs, map_memfd: only a memfd has pages to share). Skia maps font
 * files that way (sk_fdmmap in SkOSFile_posix.cpp, behind
 * SkData::MakeFromFileName) and, unlike SkStream::MakeFromFile, has no
 * fallback to reading, so SkFontMgr_fontconfig's FontAccessible() refuses every
 * font. skia.py compiles SkOSFile_posix.cpp with -Dmmap=lazyos_mmap; this is
 * the function it then calls. It maps the file for real when the kernel can,
 * and otherwise copies it into an anonymous mapping, which munmap
 * (sk_fmunmap) releases like any other. Delete this file, and the definition in
 * CMakeLists.txt, when the kernel gains file-backed mappings.
 *
 * Only "this kernel has no such mapping" falls back to copying: ENODEV (what
 * LazyOS returns) and ENOSYS. Every other error is the caller's to see, so a
 * real failure (EACCES, EBADF, ENOMEM, EINVAL from a bad offset or length)
 * is not turned into a silent copy. EPERM is not on the list either: Linux
 * gives it for a refused mapping (sealed or noexec), which a copy would
 * paper over.
 *
 * Two things this does not cover:
 *
 *  - The copy path never runs on the build host, where mmap works, so the
 *    host-versus-guest comparison of depsproof (tools/webkit/wkjudge.py) says
 *    nothing about it; only the guest's own run exercises it (the Skia text
 *    pixels are drawn from a font file read this way).
 *  - A copy holds the whole file in RAM, rounded up to pages, for as long as
 *    it is "mapped": a 20 MB CJK font costs 20 MB per mapping (the page cache
 *    shares nothing here), against the 2 GiB profile the proof runs in. Fonts
 *    WebKit loads on LazyOS must fit that budget (docs/webkit-port-plan.md).
 */
#include <errno.h>
#include <sys/mman.h>
#include <unistd.h>

static int no_file_mappings(int error) {
    return error == ENODEV || error == ENOSYS;
}

void* lazyos_mmap(void* addr, size_t length, int prot, int flags, int fd, off_t offset) {
    void* mapped = mmap(addr, length, prot, flags, fd, offset);
    if (mapped != MAP_FAILED || !no_file_mappings(errno))
        return mapped;
    /* Only the case the copy can honour: a read-only private file mapping. */
    if ((flags & MAP_ANONYMOUS) || !(flags & MAP_PRIVATE) || (prot & PROT_WRITE)) {
        errno = ENODEV;
        return MAP_FAILED;
    }
    void* copy = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (copy == MAP_FAILED)
        return MAP_FAILED;
    size_t done = 0;
    while (done < length) {
        ssize_t got = pread(fd, (char*)copy + done, length - done, offset + (off_t)done);
        if (got <= 0) {
            if (got < 0 && errno == EINTR)
                continue;
            int error = got < 0 ? errno : EIO;
            munmap(copy, length);
            errno = error;
            return MAP_FAILED;
        }
        done += (size_t)got;
    }
    if (mprotect(copy, length, prot) != 0) {
        int error = errno;
        munmap(copy, length);
        errno = error;
        return MAP_FAILED;
    }
    return copy;
}
