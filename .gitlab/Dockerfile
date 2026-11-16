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

# Pin the internal client independently of the parent image's Docker packages.
ARG BUILDX_VERSION=0.38.1-dd.1
ARG BUILDX_SHA256_amd64=3a7b0c660da5bdb9df799d5ce339bdfceeafc98b2fa7f56cf2cccbca4691a112
ARG BUILDX_SHA256_arm64=9d47a29f85bfa7b4998a4969bcb8c51102ecea1f1db19217df7b2b68977e029a
RUN set -eu; \
    buildx_arch="$(dpkg --print-architecture)"; \
    case "$buildx_arch" in \
      amd64) buildx_sha256="$BUILDX_SHA256_amd64" ;; \
      arm64) buildx_sha256="$BUILDX_SHA256_arm64" ;; \
      *) echo "Unsupported Buildx architecture: $buildx_arch" >&2; exit 1 ;; \
    esac; \
    buildx_tmp="$(mktemp -d)"; \
    trap 'rm -rf "$buildx_tmp"' EXIT; \
    curl -fsSL -o "$buildx_tmp/buildx.tar.gz" \
      "https://depot-read-api-github-releases.us1.ddbuild.io/internal/mirror/github-releases/DataDog/buildx/releases/download/v${BUILDX_VERSION}/buildx_${BUILDX_VERSION}_linux_${buildx_arch}.tar.gz"; \
    printf '%s  %s\n' "$buildx_sha256" "$buildx_tmp/buildx.tar.gz" | sha256sum -c -; \
    tar -xzf "$buildx_tmp/buildx.tar.gz" -C "$buildx_tmp" buildx; \
    install -D -m 0755 "$buildx_tmp/buildx" /usr/local/libexec/docker/cli-plugins/docker-buildx; \
    buildx_selected="$(docker buildx version)"; \
    case "$buildx_selected" in \
      "github.com/docker/buildx v${BUILDX_VERSION} "*) printf '%s\n' "$buildx_selected" ;; \
      *) echo "Unexpected Buildx client: $buildx_selected" >&2; exit 1 ;; \
    esac
