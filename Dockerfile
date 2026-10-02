ARG BUILDPLATFORM
ARG TARGETPLATFORM
ARG TARGETARCH

FROM --platform=$BUILDPLATFORM rust:1.99-bookworm@sha256:59037199c44290f2befcdd58dcc540164763fc296950255aaefeef096a1866b0 AS builder

ARG TARGETARCH

WORKDIR /app
RUN apt-get update \
	&& apt-get install -y ca-certificates pkg-config gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
	&& rm -rf /var/lib/apt/lists/*
COPY . .
RUN build_arch="${TARGETARCH:-$(dpkg --print-architecture)}" \
	&& case "$build_arch" in \
		amd64|x86_64) export RUST_TARGET=x86_64-unknown-linux-gnu ;; \
		arm64|aarch64) export RUST_TARGET=aarch64-unknown-linux-gnu ;; \
		*) echo "unsupported target architecture: $build_arch" >&2; exit 1 ;; \
	esac \
	&& rustup target add "$RUST_TARGET" \
	&& if [ "$RUST_TARGET" = "aarch64-unknown-linux-gnu" ]; then \
		export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc; \
		export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc; \
	fi \
	&& cargo build --release --manifest-path Cargo.toml --target "$RUST_TARGET" \
	&& cp "target/$RUST_TARGET/release/zcash-payment-service" /tmp/zcash-payment-service

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /tmp/zcash-payment-service /usr/local/bin/

EXPOSE 8787
ENV PORT=8787

CMD ["zcash-payment-service"]
