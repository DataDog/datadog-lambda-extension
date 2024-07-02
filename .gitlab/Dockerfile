FROM registry.ddbuild.io/images/docker:24.0.5

RUN apt-get update && apt-get install -y --fix-missing \
  curl gcc gnupg g++ make cmake unzip openssl g++

ENV PB_VERSION 25.3
ENV PB_URL https://github.com/protocolbuffers/protobuf/releases/download
ENV PB_TARGET linux-x86_64

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
  sh -s -- --profile minimal --default-toolchain nightly-x86_64-unknown-linux-gnu -y

RUN source $HOME/.cargo/env
ENV PATH /root/.cargo/bin/:$PATH

RUN rustup component add rust-src --toolchain nightly-x86_64-unknown-linux-gnu

# Install protobuy 25.3 compiler from binary for linux-86_64
RUN curl -LO ${PB_URL}/v${PB_VERSION}/protoc-${PB_VERSION}-${PB_TARGET}.zip
RUN unzip protoc-${PB_VERSION}-${PB_TARGET}.zip -d protoc
RUN mv protoc/bin/* /usr/local/bin/
RUN mv protoc/include/* /usr/local/include/
RUN rm -rf protoc*
