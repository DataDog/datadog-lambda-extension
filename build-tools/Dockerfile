FROM rust:buster as builder
WORKDIR /usr/src/app
COPY Cargo.toml .
COPY Cargo.lock .
COPY src ./src
RUN cargo build --release

FROM 486234852809.dkr.ecr.us-east-1.amazonaws.com/docker:20.10-py3
COPY --from=builder /usr/src/app/target/release/build_tools /usr/local/bin/
