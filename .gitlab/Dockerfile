FROM registry.ddbuild.io/images/docker:24.0.5

RUN apt-get update && apt-get install -y \
  curl gcc gcc-c++ make unzip openssl openssl-devel

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

RUN echo 'source $HOME/.cargo/env' >> $HOME/.bashrc
ENV PATH="$HOME/.cargo/bin:${PATH}"
