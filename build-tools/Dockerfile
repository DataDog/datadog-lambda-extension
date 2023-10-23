FROM rust:alpine as builder
WORKDIR /usr/src/app
COPY Cargo.toml .
COPY Cargo.lock .
COPY src ./src
RUN rustup target add x86_64-unknown-linux-musl
RUN cargo build --release --target=x86_64-unknown-linux-musl

FROM scratch
COPY --from=builder /usr/src/app/target/release/build_tools /build_tools
ENTRYPOINT ["/build_tools"]