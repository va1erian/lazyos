// The full modes' HTTP server (server.h).

#include "server.h"

#include <algorithm>
#include <arpa/inet.h>
#include <memory>
#include <netinet/in.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <strings.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

#include "support.h"

namespace webdtest {

namespace {

struct Connection {
    Server* server;
    int client;
};

// The value of header `name` in `head` (case-insensitive name), or "".
std::string header(const std::string& head, const char* name)
{
    const size_t length = strlen(name);
    size_t at = head.find("\r\n");
    while (at != std::string::npos && at + 2 < head.size()) {
        const size_t start = at + 2;
        const size_t end = head.find("\r\n", start);
        const std::string line = head.substr(start, end == std::string::npos ? std::string::npos : end - start);
        if (line.size() > length && !strncasecmp(line.c_str(), name, length) && line[length] == ':') {
            size_t value = length + 1;
            while (value < line.size() && line[value] == ' ')
                ++value;
            return line.substr(value);
        }
        at = end;
    }
    return std::string();
}

bool sendAll(int fd, const char* data, size_t size)
{
    while (size) {
        const ssize_t sent = send(fd, data, size, MSG_NOSIGNAL);
        if (sent <= 0)
            return false;
        data += sent;
        size -= static_cast<size_t>(sent);
    }
    return true;
}

} // namespace

Server::Server(Handler handler)
    : m_handler(std::move(handler))
{
}

Server::~Server()
{
    m_stop = true;
    if (m_started)
        pthread_join(m_thread, nullptr);
    std::vector<pthread_t> connections;
    {
        std::lock_guard<std::mutex> hold(m_mutex);
        connections = m_connections;
    }
    for (pthread_t thread : connections)
        pthread_join(thread, nullptr);
    if (m_fd >= 0)
        close(m_fd);
}

bool Server::start()
{
    m_fd = socket(AF_INET, SOCK_STREAM, 0);
    sockaddr_in address = { };
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    socklen_t length = sizeof address;
    if (m_fd < 0 || bind(m_fd, reinterpret_cast<sockaddr*>(&address), sizeof address) != 0 || listen(m_fd, 64) != 0
        || getsockname(m_fd, reinterpret_cast<sockaddr*>(&address), &length) != 0)
        return false;
    m_port = ntohs(address.sin_port);
    m_started = pthread_create(&m_thread, nullptr, &Server::acceptMain, this) == 0;
    return m_started;
}

std::string Server::origin() const { return "http://127.0.0.1:" + std::to_string(m_port); }

std::vector<Request> Server::requests()
{
    std::lock_guard<std::mutex> hold(m_mutex);
    return m_requests;
}

int Server::count(const std::string& prefix)
{
    std::lock_guard<std::mutex> hold(m_mutex);
    int n = 0;
    for (const Request& request : m_requests)
        n += request.path.compare(0, prefix.size(), prefix) == 0;
    return n;
}

Request Server::last(const std::string& path, bool& found)
{
    std::lock_guard<std::mutex> hold(m_mutex);
    found = false;
    Request result;
    for (const Request& request : m_requests) {
        if (request.path == path) {
            result = request;
            found = true;
        }
    }
    return result;
}

bool Server::wait(const std::string& prefix, int n, int ms)
{
    for (long end = monotonicMs() + ms; monotonicMs() < end; usleep(50 * 1000)) {
        if (count(prefix) >= n)
            return true;
    }
    return count(prefix) >= n;
}

std::string Server::describe()
{
    std::lock_guard<std::mutex> hold(m_mutex);
    std::string all;
    for (const Request& request : m_requests)
        all += (all.empty() ? "" : " | ") + request.method + " " + request.path + (request.cookie.empty() ? "" : " [" + request.cookie + "]");
    return all.empty() ? "(none)" : all;
}

void* Server::acceptMain(void* self)
{
    auto* server = static_cast<Server*>(self);
    while (!server->m_stop) {
        pollfd waiting = { server->m_fd, POLLIN, 0 };
        if (poll(&waiting, 1, 100) <= 0)
            continue;
        const int client = accept(server->m_fd, nullptr, nullptr);
        if (client < 0)
            continue;
        pthread_t thread;
        auto* connection = new Connection { server, client };
        if (pthread_create(&thread, nullptr, &Server::connectionMain, connection) != 0) {
            close(client);
            delete connection;
            continue;
        }
        std::lock_guard<std::mutex> hold(server->m_mutex);
        server->m_connections.push_back(thread);
    }
    return nullptr;
}

void* Server::connectionMain(void* argument)
{
    std::unique_ptr<Connection> connection(static_cast<Connection*>(argument));
    connection->server->serve(connection->client);
    close(connection->client);
    return nullptr;
}

void Server::serve(int client)
{
    timeval timeout = { 5, 0 };
    setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout);
    std::string head;
    char buffer[2048];
    while (head.find("\r\n\r\n") == std::string::npos && head.size() < 65536) {
        const ssize_t got = recv(client, buffer, sizeof buffer, 0);
        if (got <= 0)
            return;
        head.append(buffer, static_cast<size_t>(got));
    }
    Request request;
    const size_t space = head.find(' ');
    const size_t second = space == std::string::npos ? std::string::npos : head.find(' ', space + 1);
    if (second == std::string::npos)
        return;
    request.method = head.substr(0, space);
    request.path = head.substr(space + 1, second - space - 1);
    request.cookie = header(head, "Cookie");
    request.userAgent = header(head, "User-Agent");
    {
        std::lock_guard<std::mutex> hold(m_mutex);
        m_requests.push_back(request);
    }
    const Response response = m_handler(request);
    std::string reply = "HTTP/1.1 " + std::to_string(response.status) + (response.status == 200 ? " OK" : " Other") + "\r\n";
    reply += "Content-Type: " + response.type + "\r\n";
    reply += "Content-Length: " + std::to_string(response.body.size()) + "\r\n";
    reply += "Cache-Control: no-store\r\nConnection: close\r\n";
    for (const std::string& line : response.headers)
        reply += line + "\r\n";
    reply += "\r\n";
    if (!sendAll(client, reply.data(), reply.size()))
        return;
    if (!response.chunk) {
        sendAll(client, response.body.data(), response.body.size());
        return;
    }
    for (size_t at = 0; at < response.body.size() && !m_stop; at += response.chunk) {
        if (!sendAll(client, response.body.data() + at, std::min(response.chunk, response.body.size() - at)))
            return;
        usleep(static_cast<useconds_t>(response.delayMs) * 1000);
    }
}

} // namespace webdtest
