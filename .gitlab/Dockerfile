FROM registry.ddbuild.io/images/docker:24.0.5

RUN apt-get update && apt-get install -y --fix-missing --no-install-recommends \
  curl gcc gnupg g++ make cmake unzip openssl g++ uuid-runtime upx-ucl=3.96-3

# Install AWS CLI
RUN curl "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o "awscliv2.zip"
RUN unzip awscliv2.zip && ./aws/install

#  Install Protocol Buffers compiler by hand
COPY ./scripts/install-protoc.sh /
RUN chmod +x /install-protoc.sh && /install-protoc.sh

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
  sh -s -- --profile minimal --default-toolchain stable -y

RUN source $HOME/.cargo/env
ENV PATH /root/.cargo/bin/:$PATH

RUN rustup component add rust-src --toolchain stable


