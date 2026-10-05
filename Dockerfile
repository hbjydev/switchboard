# syntax=docker/dockerfile:1.27

ARG RUST_VERSION
FROM rust:${RUST_VERSION?}-slim-trixie AS builder

# TARGETARCH is supplied by buildx per platform.
ARG TARGETARCH
WORKDIR /src

COPY . .

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-${TARGETARCH},sharing=locked \
    --mount=type=cache,target=/src/target,id=cargo-target-${TARGETARCH},sharing=locked \
    RUSTFLAGS="-C target-feature=+crt-static" \
	cargo build --release --locked --bin="switchboard" --target="$(case ${TARGETARCH} in \
		"amd64") echo "x86_64";; \
		"arm64") echo "aarch64";; \
		*) echo "${TARGETARCH}";; \
	esac)-unknown-linux-gnu" && \
		cp "target/$(case ${TARGETARCH} in \
		"amd64") echo "x86_64";; \
		"arm64") echo "aarch64";; \
		*) echo "${TARGETARCH}";; \
	esac)-unknown-linux-gnu/release/switchboard" /usr/local/bin/switchboard

FROM rockylinux/rockylinux:10-ubi-micro AS runtime

RUN echo 'switchboard:x:10001:10001::/:/sbin/nologin' >> /etc/passwd \
    && echo 'switchboard:x:10001:' >> /etc/group

COPY --from=builder --chown=10001:10001 /usr/local/bin/switchboard /usr/local/bin/switchboard

USER 10001:10001
ENTRYPOINT ["/usr/local/bin/switchboard"]
CMD ["--help"]
