FROM rust
RUN curl -fsSL https://get.docker.com | sh
RUN ln -s "/var/run/docker-host.sock" "/var/run/docker.sock"
RUN rustup component add rustfmt
