# Multistage Build for Loin (x86_64 or ARM)
#
# Conditionally `cargo build` for platforms of x86_64 or ARM.
# In the final Docker stage we copy the built binary to alpine, and run with environment:
# $LND_HOST, $LND_GRPC_PORT, $TLS_FILE, $MACAROON_FILE"

# Initial build Stage 
# use nightly-slim?
FROM rustlang/rust:nightly AS builder
ARG TARGETARCH
ENV RUSTFLAGS="-Z macro-backtrace"
ENV RUST_BACKTRACE=1
WORKDIR /usr/src/loin
COPY Cargo.toml Cargo.lock build.rs config_spec.toml ./
COPY src ./src
COPY static ./static
COPY node_modules ./node_modules

# x86_64
FROM builder AS branch-version-amd64
RUN echo "Preparing to cargo build for x86_64 (${TARGETARCH})"
RUN apt-get update && apt-get install -y musl-tools musl-dev
RUN rustup target add x86_64-unknown-linux-musl
RUN cargo install --features=test_paths --target x86_64-unknown-linux-musl --path .

# ARM
FROM builder AS branch-version-arm64
RUN echo "Preparing to cargo build for arm (${TARGETARCH})"
RUN apt-get update && apt-get install -y --no-install-recommends \
    g++-arm-linux-gnueabihf \
    libc6-dev-armhf-cross
RUN rustup target add armv7-unknown-linux-gnueabihf
ENV CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc \
    CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUNNER="/linux-runner armv7hf" \
    CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc \
    CXX_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-g++ \
    BINDGEN_EXTRA_CLANG_ARGS_armv7_unknown_linux_gnueabihf="--sysroot=/usr/arm-linux-gnueabihf" \
    QEMU_LD_PREFIX=/usr/arm-linux-gnueabihf \
    RUST_TEST_THREADS=1 \
    PKG_CONFIG_PATH="/usr/lib/arm-linux-gnueabihf/pkgconfig/:${PKG_CONFIG_PATH}"
RUN cargo install --features=test_paths --target armv7-unknown-linux-gnueabihf --path .

# We want to build for either x86_64 or ARM using the above options and docker var TARGETARCH
FROM branch-version-${TARGETARCH} AS chosen_builder
RUN echo "Called build!"

# Run loin
FROM debian:buster-slim
COPY --from=chosen_builder /usr/local/cargo/bin/loin /usr/local/bin/loin
EXPOSE 4444
# We should also use: $APP_HIDDEN_SERVICE
CMD ["loin", "--bind_port", "4444", "--lnd_address=$LND_HOST:$LND_GRPC_PORT", "--lnd_cert_path=$TLS_FILE", "--lnd_macaroon_path=$MACAROON_FILE"]
