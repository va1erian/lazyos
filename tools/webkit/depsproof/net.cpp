// OpenSSL, nghttp2 and curl. The curl check is the only one that touches the
// network: an HTTPS GET of the page the host's TLS harness serves, verified
// against the image's CA bundle (which holds the harness's test CA).

#include "proof.h"

#include <curl/curl.h>
#include <nghttp2/nghttp2.h>
#include <openssl/evp.h>
#include <openssl/rand.h>
#include <openssl/ssl.h>

#include <cstring>

namespace proof {

namespace {

std::string sha256_hex(const std::string& data) {
    uint8_t digest[EVP_MAX_MD_SIZE];
    unsigned length = 0;
    EVP_MD_CTX* ctx = EVP_MD_CTX_new();
    EVP_DigestInit_ex(ctx, EVP_sha256(), nullptr);
    EVP_DigestUpdate(ctx, data.data(), data.size());
    EVP_DigestFinal_ex(ctx, digest, &length);
    EVP_MD_CTX_free(ctx);
    return hex(digest, length);
}

size_t collect(char* data, size_t size, size_t count, void* user) {
    static_cast<std::string*>(user)->append(data, size * count);
    return size * count;
}

// The CA bundle the image builds (build_support/ca_bundle.rs).
const char kCaBundle[] = "/etc/ssl/certs/ca-certificates.crt";

}  // namespace

Outcome check_openssl() {
    if (sha256_hex("abc") != "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        return fail("SHA-256 vector");
    // AES-256-GCM: seal and open, and a flipped bit must be refused.
    uint8_t key[32], iv[12], tag[16], sealed[64], opened[64];
    if (RAND_bytes(key, sizeof key) != 1 || RAND_bytes(iv, sizeof iv) != 1) return fail("RAND_bytes");
    const char plain[] = "WebKit needs TLS and SubtleCrypto";
    int length = 0, total = 0;
    EVP_CIPHER_CTX* seal = EVP_CIPHER_CTX_new();
    EVP_EncryptInit_ex(seal, EVP_aes_256_gcm(), nullptr, key, iv);
    EVP_EncryptUpdate(seal, sealed, &length, reinterpret_cast<const uint8_t*>(plain), sizeof plain);
    total = length;
    EVP_EncryptFinal_ex(seal, sealed + total, &length);
    EVP_CIPHER_CTX_ctrl(seal, EVP_CTRL_GCM_GET_TAG, sizeof tag, tag);
    EVP_CIPHER_CTX_free(seal);
    auto open = [&](bool corrupt) {
        uint8_t copy[64];
        std::memcpy(copy, sealed, sizeof plain);
        if (corrupt) copy[3] ^= 1;
        EVP_CIPHER_CTX* ctx = EVP_CIPHER_CTX_new();
        EVP_DecryptInit_ex(ctx, EVP_aes_256_gcm(), nullptr, key, iv);
        int n = 0;
        EVP_DecryptUpdate(ctx, opened, &n, copy, sizeof plain);
        EVP_CIPHER_CTX_ctrl(ctx, EVP_CTRL_GCM_SET_TAG, sizeof tag, tag);
        const bool good = EVP_DecryptFinal_ex(ctx, opened + n, &n) > 0;
        EVP_CIPHER_CTX_free(ctx);
        return good;
    };
    if (!open(false) || std::memcmp(opened, plain, sizeof plain) != 0) return fail("AES-GCM open");
    if (open(true)) return fail("AES-GCM accepted a forged message");
    SSL_CTX* tls = SSL_CTX_new(TLS_client_method());
    if (!tls) return fail("SSL_CTX_new");
    const bool v13 = SSL_CTX_set_min_proto_version(tls, TLS1_3_VERSION) == 1;
    SSL_CTX_free(tls);
    if (!v13) return fail("TLS 1.3 unavailable");
    info("openssl", "version", OpenSSL_version(OPENSSL_VERSION));
    return pass();
}

Outcome check_nghttp2() {
    nghttp2_session_callbacks* callbacks = nullptr;
    nghttp2_session_callbacks_new(&callbacks);
    nghttp2_session* session = nullptr;
    if (nghttp2_session_client_new(&session, callbacks, nullptr) != 0) {
        nghttp2_session_callbacks_del(callbacks);
        return fail("session_client_new");
    }
    nghttp2_settings_entry settings[] = {{NGHTTP2_SETTINGS_MAX_CONCURRENT_STREAMS, 100}};
    std::string problem;
    if (nghttp2_submit_settings(session, NGHTTP2_FLAG_NONE, settings, 1) != 0) problem = "submit_settings";
    // The client preface (24 bytes of magic) and its SETTINGS frame come out in pieces.
    std::string wire;
    for (;;) {
        const uint8_t* piece = nullptr;
        const nghttp2_ssize got = problem.empty() ? nghttp2_session_mem_send2(session, &piece) : 0;
        if (got <= 0) break;
        wire.append(reinterpret_cast<const char*>(piece), static_cast<size_t>(got));
    }
    static const char kMagic[] = "PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
    if (problem.empty() && (wire.size() < 24 + 9 || wire.compare(0, 24, kMagic) != 0))
        problem = "no connection preface";
    // A server reads the client's frames: our own bytes must parse back.
    nghttp2_session* server = nullptr;
    if (problem.empty()) {
        nghttp2_session_server_new(&server, callbacks, nullptr);
        const nghttp2_ssize used = nghttp2_session_mem_recv2(
            server, reinterpret_cast<const uint8_t*>(wire.data()), wire.size());
        if (used != static_cast<nghttp2_ssize>(wire.size())) problem = "server did not take the client bytes";
    }
    if (server) nghttp2_session_del(server);
    nghttp2_session_del(session);
    nghttp2_session_callbacks_del(callbacks);
    return problem.empty() ? pass() : fail(problem);
}

Outcome check_curl(const Context& context) {
    const curl_version_info_data* version = curl_version_info(CURLVERSION_NOW);
    std::string missing;
    const struct { int bit; const char* name; } wanted[] = {
        {CURL_VERSION_SSL, "SSL"}, {CURL_VERSION_LIBZ, "libz"}, {CURL_VERSION_HTTP2, "HTTP2"},
        {CURL_VERSION_BROTLI, "brotli"}, {CURL_VERSION_PSL, "PSL"}, {CURL_VERSION_ASYNCHDNS, "asyncdns"}};
    for (const auto& feature : wanted)
        if (!(version->features & feature.bit)) missing += std::string(" ") + feature.name;
    if (!missing.empty()) return fail("missing features:" + missing);

    if (curl_global_init(CURL_GLOBAL_DEFAULT) != CURLE_OK) return fail("curl_global_init");
    CURL* easy = curl_easy_init();
    if (!easy) return fail("curl_easy_init");
    std::string body, headers;
    char error[CURL_ERROR_SIZE] = "";
    curl_easy_setopt(easy, CURLOPT_URL, context.url.c_str());
    curl_easy_setopt(easy, CURLOPT_CAINFO, kCaBundle);
    curl_easy_setopt(easy, CURLOPT_WRITEFUNCTION, collect);
    curl_easy_setopt(easy, CURLOPT_WRITEDATA, &body);
    curl_easy_setopt(easy, CURLOPT_HEADERFUNCTION, collect);
    curl_easy_setopt(easy, CURLOPT_HEADERDATA, &headers);
    curl_easy_setopt(easy, CURLOPT_ERRORBUFFER, error);
    curl_easy_setopt(easy, CURLOPT_TIMEOUT, 60L);
    curl_easy_setopt(easy, CURLOPT_USERAGENT, "depsproof/1 (LazyOS WebKit dependency check)");
    curl_easy_setopt(easy, CURLOPT_ACCEPT_ENCODING, "");  // everything curl decodes: gzip, br
    const CURLcode status = curl_easy_perform(easy);
    long code = 0;
    long verify = -1;
    curl_easy_getinfo(easy, CURLINFO_RESPONSE_CODE, &code);
    curl_easy_getinfo(easy, CURLINFO_SSL_VERIFYRESULT, &verify);
    curl_easy_cleanup(easy);
    curl_global_cleanup();
    if (status != CURLE_OK) return fail(std::string("curl: ") + curl_easy_strerror(status) + " " + error);
    if (code != 200) return fail("HTTP " + std::to_string(code));
    if (verify != 0) return fail("certificate verify result " + std::to_string(verify));
    if (body.empty()) return fail("empty body");
    info("curl", "bytes", std::to_string(body.size()));
    info("curl", "sha256", sha256_hex(body).substr(0, 16));
    info("curl", "version", version->version);
    return pass();
}

}  // namespace proof
