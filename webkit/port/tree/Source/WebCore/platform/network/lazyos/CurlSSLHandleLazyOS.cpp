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

// The TLS trust anchors of the curl port on LazyOS. The PlayStation file asks its
// console's CertificateStore for the root certificates, a closed library; LazyOS
// keeps one PEM bundle (the Mozilla roots, written by the image build, docs/tls-plan.md)
// which Linux programs see at /etc/ssl/certs/ca-certificates.crt. curl is given its path,
// so certificates are verified exactly as the `curl`/`fetch` programs verify them.

#include "config.h"
#include "CurlSSLHandle.h"

#include <wtf/text/ASCIILiteral.h>
#include <wtf/text/WTFString.h>

namespace WebCore {

void CurlSSLHandle::platformInitialize()
{
    setCACertPath(String::fromLatin1("/etc/ssl/certs/ca-certificates.crt"));
}

} // namespace WebCore
