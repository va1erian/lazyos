# The LazyOS netboot registry (docs/netboot-plan.md section 5).
#
#   docker build -t lazyos-registry tools/netboot
#   docker run -d -p 8420:8420 -v lazyos-registry:/srv/lazyos lazyos-registry
#
# Standard library only: no compiler, no wheel, no pip. The build context is
# tools/netboot (see .dockerignore: the tests and the spike stay out).
FROM python:3-alpine

ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    LAZYOS_REGISTRY_BIND=0.0.0.0:8420 \
    LAZYOS_REGISTRY_ROOT=/srv/lazyos \
    LAZYOS_REGISTRY_KEYS=/srv/lazyos/keys.json \
    LAZYOS_REGISTRY_KEEP=10 \
    LAZYOS_REGISTRY_LOG=/srv/lazyos/access.log

# An unprivileged user owns the volume. A named volume copies this ownership
# the first time it is used; for a bind mount, `chown 10001:10001` the directory.
RUN adduser -D -H -u 10001 lazyos \
    && mkdir -p /srv/lazyos /app \
    && chown lazyos:lazyos /srv/lazyos

COPY *.py /app/

USER lazyos
WORKDIR /app
VOLUME /srv/lazyos
EXPOSE 8420

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD ["python", "-I", "/app/registry.py", "--healthcheck"]

ENTRYPOINT ["python", "-I", "/app/registry.py"]
