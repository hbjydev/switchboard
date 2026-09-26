# syntax=docker/dockerfile:1.27

ARG RUST_VERSION
FROM rust:${RUST_VERSION?}-slim-trixie AS builder

# TARGETARCH is supplied by buildx per platform.
ARG TARGETARCH
WORKDIR /src

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-${TARGETARCH}},sharing=locked \
    --mount=type=cache,target=/src/target,id=cargo-target-${TARGETARCH}},sharing=locked \
    RUSTFLAGS="-C target-feature=+crt-static" cargo build --release --locked --bin "switchboard" --target x86_64-unknown-linux-gnu && \
		cp target/x86_64-unknown-linux-gnu/release/switchboard /usr/local/bin/switchboard

FROM rockylinux/rockylinux:10-ubi-micro AS runtime

# renovate: datasource=github-releases depName=krallin/tini
ENV TINI_VERSION=v0.19.0
ADD https://github.com/krallin/tini/releases/download/${TINI_VERSION}/tini /usr/local/bin/tini

RUN echo 'switchboard:x:10001:10001::/:/sbin/nologin' >> /etc/passwd \
    && echo 'switchboard:x:10001:' >> /etc/group \
		&& chmod +x /usr/local/bin/tini

COPY --from=builder --chown=10001:10001 /usr/local/bin/switchboard /usr/local/bin/switchboard

USER 10001:10001
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["curl", "--max-time", "2", "--fail", "--silent", "http://127.0.0.1:8080/healthz"]

ENTRYPOINT ["/usr/local/bin/tini", "--", "/usr/local/bin/switchboard"]
