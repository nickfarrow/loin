# Build Stage 
# nightly-slim?
FROM rustlang/rust:nightly AS builder
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /usr/src/loin
#ENV OUT_DIR=/usr/src/loin/build
COPY loin.conf Cargo.toml Cargo.lock build.rs config_spec.toml ./
COPY src ./src
COPY static ./static
COPY node_modules ./node_modules
ENV RUSTFLAGS="-Z macro-backtrace"
ENV RUST_BACKTRACE=1
RUN cargo install --features=test_paths --target x86_64-unknown-linux-musl --path .

FROM alpine
COPY --from=builder /usr/local/cargo/bin/loin /home/ 
# Bundle Stage
USER 1000
# CMD ["ls", "/home/"]
CMD ["ls", "/home/loin"]

