// libxml2, expat, SQLite and libpsl.

#include "proof.h"

#include <expat.h>
#include <libpsl.h>
#include <libxml/parser.h>
#include <libxml/tree.h>
#include <libxml/xpath.h>
#include <sqlite3.h>

#include <algorithm>
#include <cstring>

namespace proof {

namespace {

const char kDocument[] =
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
    "<!DOCTYPE page [<!ENTITY name \"LazyOS\">]>\n"
    "<page lang=\"en\"><title>Hello &name;</title>"
    "<item id=\"1\">one</item><item id=\"2\">two &amp; a half</item>"
    "<svg:g xmlns:svg=\"http://www.w3.org/2000/svg\"><svg:rect width=\"3\"/></svg:g></page>";

}  // namespace

Outcome check_libxml2() {
    LIBXML_TEST_VERSION
    xmlDocPtr doc = xmlReadMemory(kDocument, sizeof kDocument - 1, "depsproof.xml", nullptr,
                                  XML_PARSE_NOENT | XML_PARSE_NONET);
    if (!doc) return fail("xmlReadMemory");
    xmlNodePtr root = xmlDocGetRootElement(doc);
    std::string problem;
    if (!root || std::strcmp(reinterpret_cast<const char*>(root->name), "page") != 0) problem = "root";
    xmlXPathContextPtr xpath = xmlXPathNewContext(doc);
    xmlXPathObjectPtr items = xpath ? xmlXPathEvalExpression(reinterpret_cast<const xmlChar*>("//item"), xpath) : nullptr;
    const int count = items && items->nodesetval ? items->nodesetval->nodeNr : -1;
    if (problem.empty() && count != 2) problem = "xpath count " + std::to_string(count);
    xmlChar* title = xmlNodeGetContent(root->children);
    if (problem.empty() && (!title || std::strcmp(reinterpret_cast<const char*>(title), "Hello LazyOS") != 0))
        problem = "entity expansion";
    xmlFree(title);
    if (items) xmlXPathFreeObject(items);
    if (xpath) xmlXPathFreeContext(xpath);
    xmlFreeDoc(doc);
    if (!problem.empty()) return fail(problem);

    // The push parser WebCore feeds chunk by chunk as the network delivers them.
    xmlParserCtxtPtr push = xmlCreatePushParserCtxt(nullptr, nullptr, nullptr, 0, "push.xml");
    if (!push) return fail("push parser");
    const size_t total = sizeof kDocument - 1;
    for (size_t at = 0; at < total; at += 7)
        if (xmlParseChunk(push, kDocument + at, static_cast<int>(std::min<size_t>(7, total - at)), 0) != 0) {
            xmlFreeParserCtxt(push);
            return fail("push chunk");
        }
    xmlParseChunk(push, nullptr, 0, 1);
    const bool good = push->wellFormed;
    xmlFreeDoc(push->myDoc);
    xmlFreeParserCtxt(push);
    xmlCleanupParser();
    return good ? pass() : fail("push parser not well formed");
}

namespace {

struct Counts {
    int elements = 0;
    int text_bytes = 0;
};

void XMLCALL on_start(void* user, const XML_Char*, const XML_Char**) { ++static_cast<Counts*>(user)->elements; }
void XMLCALL on_text(void* user, const XML_Char*, int length) { static_cast<Counts*>(user)->text_bytes += length; }

}  // namespace

Outcome check_expat() {
    Counts counts;
    XML_Parser parser = XML_ParserCreate("UTF-8");
    if (!parser) return fail("XML_ParserCreate");
    XML_SetUserData(parser, &counts);
    XML_SetElementHandler(parser, on_start, nullptr);
    XML_SetCharacterDataHandler(parser, on_text);
    const char doc[] = "<svg><g><path d=\"M0 0\"/><text>glyph</text></g></svg>";
    const bool ok = XML_Parse(parser, doc, sizeof doc - 1, 1) == XML_STATUS_OK;
    XML_ParserFree(parser);
    if (!ok) return fail("XML_Parse");
    if (counts.elements != 4 || counts.text_bytes != 5)
        return fail("counted " + std::to_string(counts.elements) + " elements");
    return pass();
}

Outcome check_sqlite() {
    sqlite3* db = nullptr;
    if (sqlite3_open(":memory:", &db) != SQLITE_OK) return fail("open");
    auto run = [&](const char* sql) {
        char* error = nullptr;
        const int status = sqlite3_exec(db, sql, nullptr, nullptr, &error);
        std::string text = error ? error : "";
        sqlite3_free(error);
        return status == SQLITE_OK ? std::string() : text.empty() ? "status " + std::to_string(status) : text;
    };
    std::string problem = run("CREATE TABLE cookie(host TEXT, name TEXT, value TEXT, PRIMARY KEY(host, name));"
                              "INSERT INTO cookie VALUES('example.org','a','1'),('example.org','b','22'),('lazy.os','a','333');"
                              "CREATE VIRTUAL TABLE page USING fts3(body);"
                              "INSERT INTO page VALUES('the quick brown fox'),('a lazy dog');");
    long long total = -1, hits = -1;
    if (problem.empty()) {
        sqlite3_stmt* statement = nullptr;
        if (sqlite3_prepare_v2(db, "SELECT sum(length(value)), (SELECT count(*) FROM page WHERE page MATCH 'lazy') "
                                   "FROM cookie WHERE host = ?1", -1, &statement, nullptr) == SQLITE_OK) {
            sqlite3_bind_text(statement, 1, "example.org", -1, SQLITE_STATIC);
            if (sqlite3_step(statement) == SQLITE_ROW) {
                total = sqlite3_column_int64(statement, 0);
                hits = sqlite3_column_int64(statement, 1);
            }
        }
        sqlite3_finalize(statement);
    }
    sqlite3_close(db);
    if (!problem.empty()) return fail(problem);
    if (total != 3 || hits != 1) return fail("query gave " + std::to_string(total) + "," + std::to_string(hits));
    info("sqlite", "version", sqlite3_libversion());
    return pass();
}

Outcome check_libpsl() {
    const psl_ctx_t* psl = psl_builtin();
    if (!psl) return fail("no built-in list");
    if (!psl_is_public_suffix(psl, "co.uk")) return fail("co.uk is a public suffix");
    if (psl_is_public_suffix(psl, "example.co.uk")) return fail("example.co.uk is not");
    const char* domain = psl_registrable_domain(psl, "www.example.co.uk");
    if (!domain || std::strcmp(domain, "example.co.uk") != 0) return fail("registrable domain");
    // Internationalised names go through ICU (the runtime this build selected).
    char* lower = nullptr;
    if (psl_str_to_utf8lower("WWW.M\xc3\x9c" "NCHEN.DE", nullptr, nullptr, &lower) != PSL_SUCCESS) return fail("utf8lower");
    const bool good = lower && std::strcmp(lower, "www.m\xc3\xbc" "nchen.de") == 0;
    psl_free_string(lower);
    return good ? pass() : fail("utf8lower result");
}

}  // namespace proof
