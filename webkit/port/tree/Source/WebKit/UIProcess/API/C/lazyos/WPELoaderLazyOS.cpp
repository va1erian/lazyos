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

// libwpe is linked statically (tools/webkit/recipes_port.py) and then asks the program for its
// backend: "a backend must define _wpe_loader_interface". The LazyOS port has no WPE backend
// (no view backend, no renderer); a backend object that is not provided is a null pointer to
// libwpe, which makes wpe_pasteboard_get_singleton() fall back to its in-process pasteboard.

#include "config.h"

#include <wpe/wpe.h>

extern "C" {

struct wpe_loader_interface _wpe_loader_interface = {
    [](const char*) -> void* { return nullptr; },
    nullptr, nullptr, nullptr, nullptr
};

}
