FROM rust as builder
WORKDIR /usr/src/app
COPY Cargo.toml .
COPY Cargo.lock .
RUN mkdir ./src && echo 'fn main() { println!("Dummy!"); }' > ./src/main.rs
RUN cargo build --release
RUN rm -rf ./src
COPY src ./src
RUN touch -a -m ./src/main.rs
RUN cargo build --release

FROM ubuntu:bionic as compresser
RUN apt-get update
RUN apt-get install -y zip
RUN mkdir -p /bin
WORKDIR /bin
COPY --from=builder /usr/src/app/target/release/build_tools /bin/build_tools
RUN zip -r build_tools.zip /bin/build_tools

#keep the smallest possible docker image
FROM scratch
COPY --from=compresser /bin/build_tools.zip /
ENTRYPOINT ["/build_tools.zip"]