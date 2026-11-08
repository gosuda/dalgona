FROM docker.io/library/buildpack-deps:stable@sha256:5214476af627916270063c9f99a969381f32cd961c861ea95e740ba71abc125f

ARG TARGETARCH

RUN useradd --create-home --uid 10001 --shell /usr/sbin/nologin dalgona \
    && mkdir -p /workspace /data \
    && chown -R dalgona:dalgona /workspace /data

COPY --chown=dalgona:dalgona dist/dalgona-linux-${TARGETARCH} /usr/local/bin/dalgona

USER dalgona
EXPOSE 7438
ENTRYPOINT ["/usr/local/bin/dalgona", "exec-server", "--listen", "ws://0.0.0.0:7438", "--root", "/workspace", "--data", "/data"]
