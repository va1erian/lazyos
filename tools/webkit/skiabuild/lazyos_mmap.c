/* A read-only private mapping of a file, for a kernel that has none.
 *
 * LazyOS has no file-backed mmap (docs/linux-abi-plan.md): mmap(fd, PROT_READ,
 * MAP_PRIVATE) fails. Skia maps font files that way (sk_fdmmap in
 * SkOSFile_posix.cpp, behind SkData::MakeFromFileName) and, unlike
 * SkStream::MakeFromFile, has no fallback to reading, so SkFontMgr_fontconfig's
 * FontAccessible() refuses every font. skia.py compiles SkOSFile_posix.cpp with
 * -Dmmap=lazyos_mmap; this is the function it then calls. It maps the file for
 * real when the kernel can, and otherwise copies it into an anonymous mapping,
 * which munmap (sk_fmunmap) releases like any other. Delete this file, and the
 * definition in CMakeLists.txt, when the kernel gains file-backed mappings.
 */
#include <errno.h>
#include <sys/mman.h>
#include <unistd.h>

void* lazyos_mmap(void* addr, size_t length, int prot, int flags, int fd, off_t offset) {
    void* mapped = mmap(addr, length, prot, flags, fd, offset);
    if (mapped != MAP_FAILED || (flags & MAP_ANONYMOUS) || !(flags & MAP_PRIVATE) || (prot & PROT_WRITE))
        return mapped;
    void* copy = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (copy == MAP_FAILED)
        return MAP_FAILED;
    size_t done = 0;
    while (done < length) {
        ssize_t got = pread(fd, (char*)copy + done, length - done, offset + (off_t)done);
        if (got <= 0) {
            if (got < 0 && errno == EINTR)
                continue;
            munmap(copy, length);
            return MAP_FAILED;
        }
        done += (size_t)got;
    }
    mprotect(copy, length, prot);
    return copy;
}
