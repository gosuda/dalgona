FROM docker.io/library/debian:stable-slim@sha256:fbfb4059d2127120aa05b6e6064523d2102552f0cf0026ddf0701c12f1f8f3e0

ARG TARGETARCH

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin dalgona \
    && mkdir -p /data/workspace /data/home \
    && chown -R dalgona:dalgona /data \
    && ln -s /data/workspace /workspace

COPY --chown=dalgona:dalgona dist/dalgona-linux-${TARGETARCH} /usr/local/bin/dalgona

USER dalgona
EXPOSE 7438
ENTRYPOINT ["/usr/local/bin/dalgona", "exec-server", "--listen", "ws://0.0.0.0:7438", "--root", "/workspace", "--data", "/data"]
