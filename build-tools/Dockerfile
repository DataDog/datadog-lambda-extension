FROM amd64/ubuntu:bionic as builder
WORKDIR /usr/src/app
RUN apt-get -y update
RUN apt-get -y install build-essential
RUN apt-get -y install openssl pkg-config libssl-dev
RUN apt-get -y install gcc
RUN apt-get -y install curl

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
RUN . $HOME/.cargo/env
COPY Cargo.toml .
COPY Cargo.lock .
RUN mkdir ./src && echo 'fn main() { println!("Dummy!"); }' > ./src/main.rs
RUN /root/.cargo/bin/cargo build --release
RUN rm -rf ./src
COPY src ./src
RUN touch -a -m ./src/main.rs
RUN /root/.cargo/bin/cargo build --release

FROM amd64/ubuntu:bionic as stripper
RUN apt-get update -y
RUN apt-get install -y binutils
COPY --from=builder /usr/src/app/target/release/build_tools /bin
RUN strip /bin/build_tools

# zip the extension
FROM amd64/ubuntu:bionic as compresser
RUN apt-get update
RUN apt-get install -y zip
RUN mkdir -p /bin
WORKDIR /bin
COPY --from=stripper /bin/build_tools /bin/build_tools
RUN zip -r build_tools.zip /bin/build_tools

#keep the smallest possible docker image
FROM scratch
COPY --from=compresser /bin/build_tools.zip /
ENTRYPOINT ["/build_tools.zip"]