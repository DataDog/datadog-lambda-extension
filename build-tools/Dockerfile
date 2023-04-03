FROM rust as builder
WORKDIR /usr/src/app
COPY Cargo.toml .
COPY Cargo.lock .
COPY src ./src
RUN cargo build --release

FROM scratch
COPY --from=builder /usr/src/app/target/release/build_tools /build_tools
ENTRYPOINT ["/build_tools"]