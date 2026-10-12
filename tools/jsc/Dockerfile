# The build host for JavaScriptCore on LazyOS (tools/jsc/README.md).
#
# Everything the build needs that is not zig: cmake, ninja, perl, python,
# ruby and gperf/bison/flex for WebKit's code generators, plus the C compiler
# of the *host* that ICU's build tools and WebKit's generators run with. zig
# (a pip wheel, the version the other tools pin) is the cross compiler for
# the LazyOS binaries. The base image is pinned by digest (debian:12-slim).
FROM debian@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential ca-certificates cmake ninja-build perl python3 python3-pip python3-venv \
      ruby gperf bison flex pkg-config xz-utils patch file git unzip \
 && rm -rf /var/lib/apt/lists/*

# zig from the pip wheel, in a venv (PEP 668); `python -m ziglang` is zig.
ARG ZIG_VERSION=0.16.0
RUN python3 -m venv /opt/venv && /opt/venv/bin/pip install --no-cache-dir ziglang==${ZIG_VERSION}
ENV PATH=/opt/venv/bin:$PATH \
    ZIG_GLOBAL_CACHE_DIR=/build/zig-cache \
    ZIG_LOCAL_CACHE_DIR=/build/zig-cache-local \
    LAZYOS_JSC_WORK=/build \
    LAZYOS_JSC_IN_CONTAINER=1

WORKDIR /work
ENTRYPOINT ["python3", "-I", "/work/tools/jsc/build.py"]
