FROM rust as builder
WORKDIR /usr/src/app
COPY Cargo.toml .
COPY Cargo.lock .
RUN cargo build --release

FROM gcr.io/distroless/static-debian11
COPY --from=builder /usr/src/app/target/release/build_tools /build_tools
ENTRYPOINT ["/build_tools"]