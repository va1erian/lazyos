/* libdynadd.so: a shared library the program links against (DT_NEEDED), with
 * an exported function, a thread-local variable (initial-exec TLS block of
 * the dynamic linker's own bookkeeping) and a constructor. */
static int constructed;

__attribute__((constructor)) static void init(void) { constructed = 7; }

int dyn_constructed(void) { return constructed; }

int dyn_add(int a, int b) { return a + b; }

__thread int dyn_tls = 41;

int dyn_tls_bump(void) { return ++dyn_tls; }
