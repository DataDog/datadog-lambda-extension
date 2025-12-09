FROM registry.ddbuild.io/images/docker:24.0.5

RUN apt-get update && apt-get install -y --fix-missing --no-install-recommends \
  curl gcc gnupg g++ make cmake unzip openssl g++ uuid-runtime libclang-dev \
  clang llvm-dev

# Install Node.js 20 from NodeSource
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - && \
  apt-get install -y nodejs

# Install AWS CLI
RUN curl "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o "awscliv2.zip"
RUN unzip awscliv2.zip && ./aws/install

#  Install Protocol Buffers compiler by hand
COPY ./scripts/install-protoc.sh /
RUN chmod +x /install-protoc.sh && /install-protoc.sh

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
  sh -s -- --profile minimal --default-toolchain stable -y

RUN source=$HOME/.cargo/env
ENV PATH=$PATH:/root/.cargo/bin/

ENV LIBCLANG_PATH=/usr/lib/x86_64-linux-gnu

RUN rustup component add rust-src --toolchain stable


