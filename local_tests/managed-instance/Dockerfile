FROM public.ecr.aws/lambda/python:3.9

# Build argument for parent directory name
ARG TARGET=managed-instance

## Set to 900 (15 minutes) for debugging - Lambda max timeout
#ENV AWS_LAMBDA_FUNCTION_TIMEOUT=900
ENV LOG_LEVEL=debug
ENV DD_LOG_LEVEL=debug
# Enforce flushing
# ENV DD_SERVERLESS_FLUSH_STRATEGY=continuously,60000
# ENV DD_SERVERLESS_FLUSH_STRATEGY=periodically,60000


# ---- Add remote dev tooling ----
# SSH server, LLDB, GDB, procps (ps/top), curl, git
RUN yum -y install lldb gdb gdb-gdbserver procps-ng tar gzip zip curl git shadow-utils sysvinit-tools\
    &&  \
    yum -y install \
          fontconfig \
          freetype \
          libXrender \
          libXext \
          libXi \
          libXtst \
          libX11 \
          alsa-lib \
          which \
    && yum clean all

# Optional: install Rust toolchain (useful if you want to build inside the container)
RUN curl https://sh.rustup.rs -sSf | bash -s -- -y
ENV PATH=/root/.cargo/bin:$PATH


# Download the AWS Lambda Runtime Interface Emulator from GitHub releases
RUN curl -Lo /usr/local/bin/aws-lambda-rie https://github.com/aws/aws-lambda-runtime-interface-emulator/releases/latest/download/aws-lambda-rie-arm64 \
    && chmod +x /usr/local/bin/aws-lambda-rie

# The first path is the location of your local agent binary
COPY .binaries/bottlecap-arm64 /opt/extensions/datadog-agent

# Copy the lambda function to the container
COPY local_tests/${TARGET}/index.py ${LAMBDA_TASK_ROOT}/

CMD ["index.handler"]

ENTRYPOINT ["/entrypoint.sh"]