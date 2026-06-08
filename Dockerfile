ARG BUILDPLATFORM
ARG TARGETPLATFORM
ARG TARGETARCH

FROM --platform=$BUILDPLATFORM rust:1.96-bookworm@sha256:13c186980fa33cc12759b429662a1322939dbe697484b7c33b47dd2698d28460 AS builder

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

FROM debian:bookworm-slim@sha256:0104b334637a5f19aa9c983a91b54c89887c0984081f2068983107a6f6c21eeb
WORKDIR /app
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /tmp/zcash-payment-service /usr/local/bin/

EXPOSE 8787
ENV PORT=8787

CMD ["zcash-payment-service"]
