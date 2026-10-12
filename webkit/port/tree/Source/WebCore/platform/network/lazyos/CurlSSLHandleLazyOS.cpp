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
 * MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO
 * EVENT SHALL THE AUTHORS OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT,
 * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
 * LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
 * PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
 * LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
 * NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE,
 * EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */

// The TLS trust anchors of the curl port on LazyOS. The PlayStation file asks its
// console's CertificateStore for the root certificates, a closed library; LazyOS
// keeps one PEM bundle (the Mozilla roots, written by the image build, docs/tls-plan.md)
// which Linux programs see at the path libs/fhs names (LAZYOS_CA_BUNDLE_PATH, set by
// tools/webkit/port_cmake.py). curl is given that path, so certificates are verified exactly as
// the `curl`/`fetch` programs verify them.

#include "config.h"
#include "CurlSSLHandle.h"

#include <wtf/text/ASCIILiteral.h>
#include <wtf/text/WTFString.h>

#ifndef LAZYOS_CA_BUNDLE_PATH
#error "LAZYOS_CA_BUNDLE_PATH must name the image's CA bundle (libs/fhs)"
#endif

namespace WebCore {

void CurlSSLHandle::platformInitialize()
{
    setCACertPath(String::fromLatin1(LAZYOS_CA_BUNDLE_PATH));
}

} // namespace WebCore
