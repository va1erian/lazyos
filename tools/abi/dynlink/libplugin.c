/* libplugin.so: loaded with dlopen at run time. It calls into libdynadd.so,
 * which the program already holds (resolved through the global scope), keeps
 * a static counter and a thread-local variable (dynamic TLS: allocated by
 * `__tls_get_addr` because the library arrived after the thread started). */
extern int dyn_add(int a, int b);

static int calls;
static __thread int plugin_tls = 100;

int plugin_run(int x) {
    calls++;
    plugin_tls += x;
    return dyn_add(plugin_tls, calls);
}

const char *plugin_name(void) { return "plugin"; }
