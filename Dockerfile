# Build Stage 
# nightly-slim?
FROM rustlang/rust:nightly AS builder
RUN rustup target add x86_64-unknown-linux-musl
RUN apt -y update
RUN apt install -y musl-tools musl-dev

WORKDIR /usr/src/loin
COPY Cargo.toml Cargo.lock build.rs config_spec.toml ./
COPY src ./src
COPY static ./static
COPY node_modules ./node_modules
ENV RUSTFLAGS="-Z macro-backtrace"
ENV RUST_BACKTRACE=1
RUN cargo install --features=test_paths --target x86_64-unknown-linux-musl --path .

# Bundle Stage
FROM alpine
COPY --from=builder /usr/local/cargo/bin/loin /home/ 
COPY loin.conf /home/
USER 1000
CMD ["/home/loin", "--conf", "/home/loin.conf"]]

