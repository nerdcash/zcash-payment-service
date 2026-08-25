ARG BUILDPLATFORM
ARG TARGETPLATFORM
ARG TARGETARCH

FROM --platform=$BUILDPLATFORM rust:1.98-bookworm@sha256:82150a52ec202c1b14d7817e14516c392bb7f5cfebd88f1ed531cb37ebd39922 AS builder

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

FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /tmp/zcash-payment-service /usr/local/bin/

EXPOSE 8787
ENV PORT=8787

CMD ["zcash-payment-service"]
