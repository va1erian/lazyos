// W1 of docs/webkit-port-plan.md: what JavaScriptCore needs from the C++
// toolchain and from LazyOS, in one static musl program.
//
// Each check prints JSC:W1:CHECK:<name>:OK or JSC:W1:FAIL:<name>:<reason>;
// the program ends with JSC:W1:PASS when every check held (exit 0), so
// tools/jsc/run.py judges it from the serial log. Built by tools/jsc/cxx.py
// with `zig c++ -target x86_64-linux-musl -static-pie` against libc++.

#include <algorithm>
#include <atomic>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <exception>
#include <filesystem>
#include <fstream>
#include <functional>
#include <iterator>
#include <map>
#include <memory>
#include <mutex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

#ifndef NO_ICU
#include <unicode/brkiter.h>
#include <unicode/locid.h>
#include <unicode/normalizer2.h>
#include <unicode/ucnv.h>
#include <unicode/uloc.h>
#include <unicode/unistr.h>
#include <unicode/ustring.h>
#include <unicode/utypes.h>
#endif

namespace fs = std::filesystem;

static int failures = 0;

static void ok(const char* name) { std::printf("JSC:W1:CHECK:%s:OK\n", name); }
static void fail(const char* name, const std::string& why) {
    ++failures;
    std::printf("JSC:W1:FAIL:%s:%s\n", name, why.c_str());
}
#define CHECK(name, cond, why) do { if (cond) ok(name); else fail(name, why); } while (0)

// ---- threads ---------------------------------------------------------------

static void threads() {
    constexpr int kThreads = 8, kRounds = 20000;
    std::mutex lock;
    long guarded = 0;
    std::atomic<long> atomic{0};
    std::vector<std::thread> pool;
    for (int t = 0; t < kThreads; ++t)
        pool.emplace_back([&] {
            for (int i = 0; i < kRounds; ++i) {
                std::lock_guard<std::mutex> hold(lock);
                ++guarded;
                atomic.fetch_add(1, std::memory_order_relaxed);
            }
        });
    for (auto& t : pool) t.join();
    CHECK("mutex", guarded == (long)kThreads * kRounds && atomic == guarded,
          "lost updates: " + std::to_string(guarded));

    // A condition variable hand-off through a queue, the pattern of a GC's
    // worker threads and of a task queue.
    std::mutex m;
    std::condition_variable cv;
    std::vector<int> queue;
    bool done = false;
    long sum = 0;
    std::thread consumer([&] {
        std::unique_lock<std::mutex> hold(m);
        for (;;) {
            cv.wait(hold, [&] { return !queue.empty() || done; });
            while (!queue.empty()) { sum += queue.back(); queue.pop_back(); }
            if (done) return;
        }
    });
    for (int i = 1; i <= 1000; ++i) {
        { std::lock_guard<std::mutex> hold(m); queue.push_back(i); }
        cv.notify_one();
    }
    { std::lock_guard<std::mutex> hold(m); done = true; }
    cv.notify_all();
    consumer.join();
    CHECK("condvar", sum == 500500, "sum " + std::to_string(sum));

    std::thread::id main_id = std::this_thread::get_id(), other;
    std::thread([&] { other = std::this_thread::get_id(); }).join();
    CHECK("thread_id", main_id != other, "ids equal");
    CHECK("hardware_concurrency", std::thread::hardware_concurrency() >= 1, "zero");
}

// ---- exceptions ------------------------------------------------------------

struct Boom : std::runtime_error {
    int code;
    Boom(const char* what, int c) : std::runtime_error(what), code(c) {}
};

static int depth(int n) {
    if (n == 0) throw Boom("bottom", 42);
    std::string guard = "frame" + std::to_string(n);  // destructor runs while unwinding
    return depth(n - 1) + (int)guard.size();
}

static void exceptions() {
    int code = 0;
    try { depth(64); } catch (const Boom& e) { code = e.code; }
    CHECK("throw_catch", code == 42, "code " + std::to_string(code));

    std::string nested;
    try {
        try { throw std::out_of_range("inner"); }
        catch (...) { std::throw_with_nested(std::runtime_error("outer")); }
    } catch (const std::exception& e) {
        nested = e.what();
        try { std::rethrow_if_nested(e); } catch (const std::exception& in) { nested += "/"; nested += in.what(); }
    }
    CHECK("nested", nested == "outer/inner", nested);

    // An exception that crosses a thread boundary by exception_ptr.
    std::exception_ptr carried;
    std::thread([&] {
        try { throw Boom("from thread", 7); } catch (...) { carried = std::current_exception(); }
    }).join();
    int seen = 0;
    try { std::rethrow_exception(carried); } catch (const Boom& e) { seen = e.code; }
    CHECK("exception_ptr", seen == 7, "code " + std::to_string(seen));

    bool bad_alloc = false;
    try { std::vector<char> v(std::size_t(1) << 62); } catch (const std::bad_alloc&) { bad_alloc = true; }
    catch (const std::length_error&) { bad_alloc = true; }
    CHECK("bad_alloc", bad_alloc, "no exception");

    bool dyn = false;
    struct A { virtual ~A() = default; };
    struct B : A {};
    std::unique_ptr<A> a(new B);
    dyn = dynamic_cast<B*>(a.get()) != nullptr && typeid(*a) == typeid(B);
    CHECK("rtti", dyn, "dynamic_cast failed");
}

// ---- thread-local storage --------------------------------------------------

static std::atomic<int> tls_destroyed{0};
struct TlsProbe {
    int value;
    explicit TlsProbe(int v = 0) : value(v) {}
    ~TlsProbe() { tls_destroyed.fetch_add(1); }
};
static thread_local TlsProbe tls_probe(5);
static thread_local std::string tls_text;
static thread_local int tls_counter = 100;

static void tls() {
    constexpr int kThreads = 6;
    std::atomic<int> good{0};
    std::vector<std::thread> pool;
    for (int t = 0; t < kThreads; ++t)
        pool.emplace_back([&, t] {
            tls_probe.value += t;
            tls_text = "thread " + std::to_string(t);
            tls_counter += t;
            std::this_thread::yield();
            if (tls_probe.value == 5 + t && tls_text == "thread " + std::to_string(t) &&
                tls_counter == 100 + t)
                good.fetch_add(1);
        });
    for (auto& t : pool) t.join();
    CHECK("tls_isolated", good == kThreads, std::to_string(good.load()) + " of 6");
    CHECK("tls_destructors", tls_destroyed.load() == kThreads,
          "ran " + std::to_string(tls_destroyed.load()));
    CHECK("tls_main", tls_probe.value == 5 && tls_counter == 100 && tls_text.empty(),
          "main thread's copy changed");
}

// ---- std::filesystem -------------------------------------------------------

static void filesystem() {
    std::error_code ec;
    fs::path root = fs::temp_directory_path(ec) / "jsc-w1";
    fs::remove_all(root, ec);
    CHECK("fs_temp", !ec, ec.message());
    bool made = fs::create_directories(root / "a" / "b", ec);
    CHECK("fs_mkdirs", made && !ec, ec.message());
    {
        std::ofstream f(root / "a" / "one.txt");
        f << "hello filesystem\n";
        std::ofstream g(root / "a" / "b" / "two.txt");
        g << "x";
    }
    CHECK("fs_size", fs::file_size(root / "a" / "one.txt", ec) == 17 && !ec, ec.message());
    std::vector<std::string> seen;
    for (auto& e : fs::recursive_directory_iterator(root, ec))
        seen.push_back(e.path().lexically_relative(root).string());
    std::sort(seen.begin(), seen.end());
    std::string joined;
    for (auto& s : seen) joined += s + ";";
    CHECK("fs_walk", joined == "a;a/b;a/b/two.txt;a/one.txt;", joined);
    fs::rename(root / "a" / "one.txt", root / "a" / "renamed.txt", ec);
    CHECK("fs_rename", !ec && fs::exists(root / "a" / "renamed.txt") &&
                           !fs::exists(root / "a" / "one.txt"), ec.message());
    fs::copy_file(root / "a" / "renamed.txt", root / "copy.txt", ec);
    CHECK("fs_copy", !ec && fs::file_size(root / "copy.txt") == 17, ec.message());
    auto status = fs::status(root / "a", ec);
    CHECK("fs_status", fs::is_directory(status) && !fs::is_regular_file(status), "not a directory");
    fs::path canon = fs::weakly_canonical(root / "a" / ".." / "a" / "b", ec);
    CHECK("fs_canonical", canon == root / "a" / "b", canon.string());
    fs::path cwd = fs::current_path(ec);
    fs::current_path(root, ec);
    CHECK("fs_chdir", !ec && fs::current_path() == root, ec.message());
    fs::current_path(cwd, ec);
    auto removed = fs::remove_all(root, ec);
    CHECK("fs_remove_all", !ec && removed == 6 && !fs::exists(root), "removed " + std::to_string(removed));
}

// ---- ICU -------------------------------------------------------------------

#ifndef NO_ICU
static void unicode_checks() {
    // Data lives in the library, found by symbol: any of these fails without it.
    UErrorCode status = U_ZERO_ERROR;
    char name[ULOC_FULLNAME_CAPACITY];
    uloc_canonicalize("en_us", name, sizeof name, &status);
    CHECK("uloc_canonicalize", U_SUCCESS(status) && std::strcmp(name, "en_US") == 0, name);

    status = U_ZERO_ERROR;
    UChar display[64];
    int32_t n = uloc_getDisplayLanguage("fr", "en", display, 64, &status);
    icu::UnicodeString shown(display, n);
    std::string utf8;
    shown.toUTF8String(utf8);
    CHECK("uloc_display", U_SUCCESS(status) && utf8 == "French", utf8 + " " + u_errorName(status));

    // A legacy converter: its tables come from the data.
    status = U_ZERO_ERROR;
    UConverter* cnv = ucnv_open("windows-1252", &status);
    bool cnv_ok = U_SUCCESS(status);
    if (cnv_ok) {
        const char bytes[] = {char(0x80), 'a', char(0xE9)};  // euro, a, e-acute
        UChar out[8];
        int32_t len = ucnv_toUChars(cnv, out, 8, bytes, 3, &status);
        cnv_ok = U_SUCCESS(status) && len == 3 && out[0] == 0x20AC && out[2] == 0xE9;
        ucnv_close(cnv);
    }
    CHECK("ucnv_windows1252", cnv_ok, std::string("status ") + u_errorName(status));

    // Case mapping with a locale (Turkish dotted/dotless i).
    icu::UnicodeString up = icu::UnicodeString::fromUTF8("istanbul").toUpper(icu::Locale("tr"));
    std::string up8;
    up.toUTF8String(up8);
    CHECK("tr_upper", up8 == "\xC4\xB0STANBUL", up8);

    // Normalisation tables.
    status = U_ZERO_ERROR;
    const icu::Normalizer2* nfd = icu::Normalizer2::getNFDInstance(status);
    icu::UnicodeString decomposed = nfd->normalize(icu::UnicodeString((UChar)0xE9), status);
    CHECK("nfd", U_SUCCESS(status) && decomposed.length() == 2 && decomposed[1] == 0x301,
          u_errorName(status));

    // Word break rules and dictionary-free data (brkitr).
    status = U_ZERO_ERROR;
    std::unique_ptr<icu::BreakIterator> words(
        icu::BreakIterator::createWordInstance(icu::Locale::getUS(), status));
    int count = 0;
    if (U_SUCCESS(status)) {
        icu::UnicodeString text = icu::UnicodeString::fromUTF8("one two  three");
        words->setText(text);
        for (int32_t b = words->first(); b != icu::BreakIterator::DONE; b = words->next()) ++count;
    }
    CHECK("break_words", U_SUCCESS(status) && count == 6, u_errorName(status) + std::string(" ") + std::to_string(count));
}
#endif

int main() {
    std::setvbuf(stdout, nullptr, _IOLBF, 0);
    std::printf("JSC:W1:UP\n");
    threads();
    exceptions();
    tls();
    filesystem();
    if (failures == 0) std::printf("JSC:W1:PASS:cxx\n");
#ifndef NO_ICU
    int before = failures;
    unicode_checks();
    if (failures == before) std::printf("JSC:W1:PASS:icu\n");
#endif
    if (failures == 0) { std::printf("JSC:W1:PASS\n"); return 0; }
    std::printf("JSC:W1:FAIL:summary:%d checks\n", failures);
    return 1;
}
