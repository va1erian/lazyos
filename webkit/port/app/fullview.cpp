// A full view's own configuration (webd, docs/webd-plan.md 3.5; ViewConfig::configured): what the owner set
// besides the features. A browser view of LazyWeb+ never comes here, so its preferences stay WebKit's.
//
//  * scripts off: WKPreferences, as a light view does it; on, the preferences are not touched;
//  * the default font size: the default and the minimum, as for a light view;
//  * prefers-color-scheme: compiled out of this WebKit (light.cpp): said, not failed;
//  * the user style: the C API has no user style sheet, so a user script at the document's end adds a <style>
//    to the main frame; without scripts it cannot, and says so;
//  * the user agent suffix: WebKit's user agent, a space and the suffix, as the page's custom one; site-specific
//    quirks off for that view (they put a fixed user agent on every request);
//  * the owner's own HTML (loadFullHtml) under an about:, data:, http: or https: base, never a file: one;
//  * no network (ViewConfig::noNetwork): the light views' lock list on the page's controller.

#include <WebKit/WKData.h>
#include <WebKit/WKPage.h>
#include <WebKit/WKPageConfigurationRef.h>
#include <WebKit/WKPreferencesRef.h>
#include <WebKit/WKPreferencesRefPrivate.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKUserContentControllerRef.h>
#include <WebKit/WKURL.h>
#include <WebKit/WKUserScriptRef.h>

#include <ctype.h>
#include <stdint.h>
#include <stdio.h>
#include <string>

#include "engine.h"

namespace webkitapp {

namespace {

// `css` as a JavaScript string literal: nothing in it can end the literal or the script element.
std::string scriptString(const std::string& css)
{
    std::string out = "\"";
    for (unsigned char c : css) {
        if (c == '"' || c == '\\') {
            out += '\\';
            out += static_cast<char>(c);
        } else if (c < 0x20 || c == '<' || c == '>') {
            char escaped[8];
            snprintf(escaped, sizeof escaped, "\\u%04x", c);
            out += escaped;
        } else {
            out += static_cast<char>(c);
        }
    }
    // U+2028 and U+2029 end a line in older parsers: their UTF-8 bytes pass through as text, which is valid
    // in a string literal since ES2019 (JavaScriptCore's).
    return out + "\"";
}

// The page's user content controller: the blocking one when the view has it (blocking.cpp made it), else one
// made here for the page (the configuration keeps it), once.
class Controller {
public:
    Controller(View& v, WKPageConfigurationRef configuration)
        : m_controller(v.controller)
        , m_configuration(configuration)
    {
    }
    ~Controller()
    {
        if (m_own)
            WKRelease(m_controller);
    }
    WKUserContentControllerRef get()
    {
        if (!m_controller) {
            m_controller = WKUserContentControllerCreate();
            m_own = true;
            WKPageConfigurationSetUserContentController(m_configuration, m_controller);
        }
        return m_controller;
    }

private:
    WKUserContentControllerRef m_controller;
    WKPageConfigurationRef m_configuration;
    bool m_own { false };
};

void addUserStyle(View& v, Controller& controller)
{
    const std::string source = "(function(){var s=document.createElement('style');s.textContent=" + scriptString(v.config.userStyle)
        + ";(document.head||document.documentElement).appendChild(s);})()";
    WKStringRef text = WKStringCreateWithUTF8CString(source.c_str());
    WKUserScriptRef script = WKUserScriptCreateWithSource(text, kWKInjectAtDocumentEnd, true);
    WKUserContentControllerAddUserScript(controller.get(), script);
    WKRelease(script);
    WKRelease(text);
}

} // namespace

bool configureFull(View& v, WKPageConfigurationRef configuration)
{
    const ViewConfig& config = v.config;
    Controller controller(v, configuration);
    if (config.noNetwork) {
        // The light views' lock, on this page's controller before the page exists: no request is made before it.
        // The blocking lists rebuild the controller's filters at every navigation (blocking.cpp) and would drop
        // it, so the two never meet; and without the lock the view is not made (fail closed).
        WKUserContentFilterRef lock = lightLockFilter();
        if (!lock || v.controller) {
            say("FAIL:view:a full view without network needs the ready lock and no content blocking");
            return false;
        }
        WKUserContentControllerAddUserContentFilter(controller.get(), lock);
    }
    const bool ownAgent = !config.userAgentSuffix.empty() || !config.userAgent.empty();
    if (!config.scripts || config.defaultFontSize > 0 || ownAgent) {
        WKPreferencesRef preferences = WKPreferencesCreate();
        WKPreferencesSetJavaScriptEnabled(preferences, config.scripts);
        // With site-specific quirks the page's process sends PlayStation's fixed user agent for every address
        // (WebPagePlayStation.cpp, platformUserAgent -> standardUserAgentForURL), whatever the page's own is: a
        // suffix or an agent would never reach a request. A view that asks for one does without the quirks.
        if (ownAgent)
            WKPreferencesSetNeedsSiteSpecificQuirks(preferences, false);
        if (config.defaultFontSize > 0) {
            WKPreferencesSetDefaultFontSize(preferences, static_cast<uint32_t>(config.defaultFontSize));
            WKPreferencesSetMinimumFontSize(preferences, static_cast<uint32_t>(config.defaultFontSize));
        }
        WKPageConfigurationSetPreferences(configuration, preferences);
        WKRelease(preferences);
    }
    if (config.colorScheme == 1)
        say("CONFIG:IGNORED:color_scheme");
    if (!config.userStyle.empty()) {
        if (config.scripts)
            addUserStyle(v, controller);
        else
            say("CONFIG:IGNORED:user_style");
    }
    return true;
}

void loadFullHtml(View& v, const std::string& html, const std::string& baseUrl)
{
    WKURLRef base = WKURLCreateWithUTF8CString(baseUrl.empty() ? "about:blank" : baseUrl.c_str());
    WKStringRef schemeText = WKURLCopyScheme(base);
    std::string scheme = utf8(schemeText);
    if (schemeText)
        WKRelease(schemeText);
    for (char& c : scheme)
        c = static_cast<char>(tolower(static_cast<unsigned char>(c)));
    // The owner's page under an origin the service checked: an inert one or a web one, never a file (the page
    // would read the disk) or any other scheme. WebKit's parse decides the scheme, not the caller's spelling.
    if (scheme != "about" && scheme != "data" && scheme != "http" && scheme != "https") {
        WKRelease(base);
        say("FULL:BASE:REFUSED");
        v.error = "The page was not loaded: its base URL must be about:, data:, http: or https:.";
        ++v.stateSerial;
        postState(v);
        return;
    }
    // Bytes, as the light path loads them (a WKString would end at the first NUL); the navigation it starts
    // is asked about (webd_ui_navigate) like any other.
    WKDataRef data = WKDataCreate(reinterpret_cast<const unsigned char*>(html.data()), html.size());
    WKStringRef mime = WKStringCreateWithUTF8CString("text/html");
    WKStringRef encoding = WKStringCreateWithUTF8CString("UTF-8");
    WKPageLoadData(v.page, data, mime, encoding, base);
    WKRelease(encoding);
    WKRelease(mime);
    WKRelease(data);
    WKRelease(base);
}

void applyUserAgent(View& v, const std::string& agentOverride)
{
    // The first call is when the page is made, before any custom agent is set.
    if (v.standardAgent.empty()) {
        WKStringRef standard = WKPageCopyUserAgent(v.page);
        v.standardAgent = utf8(standard);
        if (standard)
            WKRelease(standard);
    }
    std::string agent = agentOverride;
    if (agent.empty())
        agent = v.config.userAgent;
    // WebKit's own user agent with the suffix after a space, as the custom one: WKPageSetApplicationNameForUserAgent
    // would add "/<WebKit's version>" to it (UserAgentPlayStation.cpp), which is not what the owner wrote.
    if (agent.empty() && !v.config.userAgentSuffix.empty())
        agent = v.standardAgent + " " + v.config.userAgentSuffix;
    // An empty custom agent is WebKit's own again.
    WKStringRef custom = WKStringCreateWithUTF8CString(agent.c_str());
    WKPageSetCustomUserAgent(v.page, custom);
    WKRelease(custom);
}

} // namespace webkitapp
