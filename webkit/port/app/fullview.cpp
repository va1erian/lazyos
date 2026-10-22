// A full view's own configuration (webd, docs/webd-plan.md 3.5; ViewConfig::configured): what the owner set
// besides the features. A browser view of LazyWeb+ never comes here, so its preferences stay WebKit's.
//
//  * scripts off: WKPreferences, as a light view does it; on, the preferences are not touched;
//  * the default font size: the default and the minimum, as for a light view;
//  * prefers-color-scheme: compiled out of this WebKit (light.cpp): said, not failed;
//  * the user style: the C API has no user style sheet, so a user script at the document's end adds a <style>
//    to the main frame; without scripts it cannot, and says so;
//  * the user agent suffix: WebKit's user agent, a space and the suffix, as the page's custom one; site-specific
//    quirks off for that view (they put a fixed user agent on every request).

#include <WebKit/WKPage.h>
#include <WebKit/WKPageConfigurationRef.h>
#include <WebKit/WKPreferencesRef.h>
#include <WebKit/WKPreferencesRefPrivate.h>
#include <WebKit/WKString.h>
#include <WebKit/WKType.h>
#include <WebKit/WKUserContentControllerRef.h>
#include <WebKit/WKUserScriptRef.h>

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

void addUserStyle(View& v, WKPageConfigurationRef configuration)
{
    const std::string source = "(function(){var s=document.createElement('style');s.textContent=" + scriptString(v.config.userStyle)
        + ";(document.head||document.documentElement).appendChild(s);})()";
    // The blocking controller when the view has one (blocking.cpp made it), else one of its own.
    WKUserContentControllerRef controller = v.controller;
    const bool own = !controller;
    if (own) {
        controller = WKUserContentControllerCreate();
        WKPageConfigurationSetUserContentController(configuration, controller);
    }
    WKStringRef text = WKStringCreateWithUTF8CString(source.c_str());
    WKUserScriptRef script = WKUserScriptCreateWithSource(text, kWKInjectAtDocumentEnd, true);
    WKUserContentControllerAddUserScript(controller, script);
    WKRelease(script);
    WKRelease(text);
    if (own)
        WKRelease(controller);
}

} // namespace

void configureFull(View& v, WKPageConfigurationRef configuration)
{
    const ViewConfig& config = v.config;
    if (!config.scripts || config.defaultFontSize > 0 || !config.userAgentSuffix.empty()) {
        WKPreferencesRef preferences = WKPreferencesCreate();
        WKPreferencesSetJavaScriptEnabled(preferences, config.scripts);
        // With site-specific quirks the page's process sends PlayStation's fixed user agent for every address
        // (WebPagePlayStation.cpp, platformUserAgent -> standardUserAgentForURL), whatever the page's own is: a
        // suffix would never reach a request. A view that asks for one does without the quirks.
        if (!config.userAgentSuffix.empty())
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
            addUserStyle(v, configuration);
        else
            say("CONFIG:IGNORED:user_style");
    }
}

void applyUserAgentSuffix(View& v)
{
    // WebKit's own user agent with the suffix after a space, as the custom one: WKPageSetApplicationNameForUserAgent
    // would add "/<WebKit's version>" to it (UserAgentPlayStation.cpp), which is not what the owner wrote.
    WKStringRef standard = WKPageCopyUserAgent(v.page);
    const std::string agent = utf8(standard) + " " + v.config.userAgentSuffix;
    if (standard)
        WKRelease(standard);
    WKStringRef custom = WKStringCreateWithUTF8CString(agent.c_str());
    WKPageSetCustomUserAgent(v.page, custom);
    WKRelease(custom);
}

} // namespace webkitapp
