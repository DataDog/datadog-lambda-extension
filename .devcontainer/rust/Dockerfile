FROM public.ecr.aws/lambda/provided:al2023 as builder
WORKDIR /usr/src/app

ENV PATH=$PATH:/root/.cargo/bin/

RUN dnf -y install gcc
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
RUN source $HOME/.cargo/env
RUN rustup component add rustfmt

ENTRYPOINT "/bin/bash"