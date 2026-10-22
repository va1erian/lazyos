// A small HTTP/1.1 server on 127.0.0.1 for the full modes: it answers each request with what the mode's handler
// says (one connection per request, Connection: close, never cached) and keeps every request it saw, with the
// headers a mode judges (Cookie, User-Agent). A slow answer is sent in pieces and stops when the server does.

#pragma once

#include <atomic>
#include <functional>
#include <mutex>
#include <pthread.h>
#include <string>
#include <vector>

namespace webdtest {

struct Request {
    std::string method, path, cookie, userAgent;
};

struct Response {
    int status { 200 };
    std::string type { "text/html; charset=utf-8" };
    // Extra header lines, without CRLF ("Set-Cookie: a=b").
    std::vector<std::string> headers;
    std::string body;
    // > 0: the body is sent `chunk` bytes at a time, `delayMs` apart.
    size_t chunk { 0 };
    int delayMs { 0 };
};

class Server {
public:
    using Handler = std::function<Response(const Request&)>;
    explicit Server(Handler handler);
    ~Server();
    Server(const Server&) = delete;
    Server& operator=(const Server&) = delete;

    bool start();
    int port() const { return m_port; }
    // "http://127.0.0.1:<port>"
    std::string origin() const;
    std::vector<Request> requests();
    // How many requests asked for a path that starts with `prefix`.
    int count(const std::string& prefix);
    // The last request for exactly `path`, if any (`found` says).
    Request last(const std::string& path, bool& found);
    // Waits until `count(prefix) >= n` or `ms` pass.
    bool wait(const std::string& prefix, int n, int ms);
    // Every request, for a failure message.
    std::string describe();

private:
    static void* acceptMain(void* self);
    static void* connectionMain(void* argument);
    void serve(int client);

    Handler m_handler;
    int m_fd { -1 };
    int m_port { 0 };
    std::atomic<bool> m_stop { false };
    bool m_started { false };
    pthread_t m_thread { };
    std::mutex m_mutex;
    std::vector<Request> m_requests;
    std::vector<pthread_t> m_connections;
};

} // namespace webdtest
