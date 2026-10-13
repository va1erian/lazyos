/* dynlink: a dynamically linked musl program (issue #535, gap 1).
 *
 * It runs on the interpreter the kernel maps from PT_INTERP (musl's
 * ld-musl-x86_64.so.1, at /lib/ and so at /system/lib), links libdynadd.so at
 * start, dlopens libplugin.so, uses thread-local storage of both, starts a
 * thread, and (as the non-PIE `dynlink`) runs the same checks again in its PIE
 * twin `dynpie` as a child.
 *
 * Output: `ABI:dynlink:PASS` or `ABI:dynlink:FAIL:<reason>`.
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <elf.h>
#include <errno.h>
#include <link.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/wait.h>
#include <unistd.h>

extern int dyn_add(int a, int b);
extern int dyn_constructed(void);
extern __thread int dyn_tls;
extern int dyn_tls_bump(void);

static char reason[160];

static int fail(const char *why) {
    snprintf(reason, sizeof reason, "%s", why);
    return 1;
}

static int count_object(struct dl_phdr_info *info, size_t size, void *data) {
    (void)info;
    (void)size;
    ++*(int *)data;
    return 0;
}

static void *thread_main(void *arg) {
    /* A new thread has its own copy of every TLS variable. */
    int (*run)(int) = (int (*)(int))arg;
    long result = dyn_tls_bump();       /* 41 + 1 */
    result = result * 1000 + run(7);    /* plugin_tls 100 + 7, calls 3 */
    return (void *)result;
}

static int checks(void) {
    if (getauxval(AT_BASE) == 0)
        return fail("AT_BASE is zero");
    if (dyn_add(2, 3) != 5)
        return fail("dyn_add through the PLT");
    if (dyn_constructed() != 7)
        return fail("a DT_NEEDED library's constructor did not run");
    if (dyn_tls != 41)
        return fail("a shared library's TLS initial value");
    if (dyn_tls_bump() != 42 || dyn_tls != 42)
        return fail("TLS store");

    int objects = 0;
    dl_iterate_phdr(count_object, &objects);
    if (objects < 3)
        return fail("dl_iterate_phdr saw fewer than the program, libdynadd and libc");

    void *plugin = dlopen("libplugin.so", RTLD_NOW | RTLD_GLOBAL);
    if (!plugin) {
        snprintf(reason, sizeof reason, "dlopen libplugin.so: %s", dlerror());
        return 1;
    }
    int (*run)(int) = (int (*)(int))dlsym(plugin, "plugin_run");
    const char *(*name)(void) = (const char *(*)(void))dlsym(plugin, "plugin_name");
    if (!run || !name)
        return fail("dlsym");
    if (strcmp(name(), "plugin") != 0)
        return fail("plugin_name");
    if (run(7) != 108)      /* plugin_tls 107, calls 1: dyn_add(107, 1) */
        return fail("plugin_run first call");
    if (run(7) != 116)      /* 114 + 2 */
        return fail("plugin_run second call");

    pthread_t thread;
    void *result = 0;
    if (pthread_create(&thread, 0, thread_main, (void *)run) != 0)
        return fail("pthread_create");
    pthread_join(thread, &result);
    /* dyn_tls 42 in the new thread, plugin_tls 107 + calls 3 = 110. */
    if ((long)result != 42L * 1000 + 110L)
        return fail("a thread's TLS or the plugin call from it");
    if (dyn_tls != 42)
        return fail("a thread's TLS store reached the main thread");

    if (dlopen("libnosuchthing.so", RTLD_NOW) != 0 || dlerror() == 0)
        return fail("a missing library was found");
    if (dlclose(plugin) != 0)
        return fail("dlclose");
    return 0;
}

int main(int argc, char **argv) {
    int status = checks();
    if (argc > 1)
        return status;      /* the child run: the parent reports */
    if (status) {
        printf("ABI:dynlink:FAIL:%s\n", reason);
        return 1;
    }
    /* The PIE twin, as a child: loaded at the PIE base, same interpreter. */
    pid_t pid = fork();
    if (pid == 0) {
        char *args[] = {"dynpie", "child", 0};
        execv("/system/bin/dynpie", args);
        _exit(127);
    }
    int wstatus = 0;
    if (pid < 0 || waitpid(pid, &wstatus, 0) != pid || !WIFEXITED(wstatus) ||
        WEXITSTATUS(wstatus) != 0) {
        printf("ABI:dynlink:FAIL:the PIE twin failed (status %d)\n", wstatus);
        return 1;
    }
    printf("ABI:dynlink:PASS\n");
    return 0;
}
