FROM registry.ddbuild.io/images/docker:24.0.5

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
RUN echo 'source $HOME/.cargo/env' >> $HOME/.bashrc
ENV PATH="$HOME/.cargo/bin:${PATH}"
