# syntax=docker/dockerfile:1.3-labs

FROM rust AS build

RUN cargo new /app/build-tools
COPY Cargo.toml /app/build-tools

WORKDIR /app/build-tools
RUN --mount=type=cache,target=/usr/local/cargo/registry cargo build --release

COPY ./src /app/build-tools/src

RUN --mount=type=cache,target=/usr/local/cargo/registry <<EOF
  set -e
  touch /app/build-tools/src/main.rs
  cargo build --release
EOF

FROM ubuntu:jammy as compresser
RUN apt-get update
RUN apt-get install -y zip
RUN mkdir -p /bin
WORKDIR /bin
COPY --from=build /app/build-tools/target/release/build_tools /bin/build_tools
RUN zip -r build_tools.zip /bin/build_tools

#keep the smallest possible docker image
FROM scratch
COPY --from=compresser /bin/build_tools.zip /
ENTRYPOINT ["/build_tools.zip"]