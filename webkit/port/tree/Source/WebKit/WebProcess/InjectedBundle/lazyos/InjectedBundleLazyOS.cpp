/*
 * Copyright (C) 2026 LazyOS contributors
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE AUTHORS ``AS IS'' AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
 */

// Injected bundles on LazyOS: not supported. A bundle is a shared library the WebProcess
// loads and calls WKBundleInitialize in (the PlayStation file does that through its closed
// LibraryBundle). The port's processes are static executables, which have no dynamic loader,
// so a context that names a bundle gets a clear refusal instead of a missing symbol.

#include "config.h"
#include "InjectedBundle.h"

#include <stdio.h>

namespace WebKit {

bool InjectedBundle::initialize(const WebProcessCreationParameters&, RefPtr<API::Object>&&)
{
    fprintf(stderr, "WebKit: injected bundles are not supported (the WebProcess is a static executable): %s\n",
        m_path.utf8().data());
    return false;
}

void InjectedBundle::setBundleParameter(WTF::String const&, std::span<const uint8_t>)
{
}

void InjectedBundle::setBundleParameters(std::span<const uint8_t>)
{
}

} // namespace WebKit
