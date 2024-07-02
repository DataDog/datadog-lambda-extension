FROM registry.ddbuild.io/images/docker:24.0.5

ARG PLATFORM
RUN apt-get update && apt-get install -y --fix-missing \
  curl gcc gnupg g++ make cmake unzip openssl g++

#  Install Protocol Buffers compiler by hand
COPY ./scripts/install-protoc.sh /
RUN chmod +x /install-protoc.sh && /install-protoc.sh

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
  sh -s -- --profile minimal --default-toolchain nightly-$PLATFORM-unknown-linux-gnu -y

RUN source $HOME/.cargo/env
ENV PATH /root/.cargo/bin/:$PATH

RUN rustup component add rust-src --toolchain nightly-$PLATFORM-unknown-linux-gnu


