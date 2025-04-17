#!/bin/bash

set -ex

# Setup cleanup trap to ensure docker container is stopped and removed even if script is interrupted
cleanup() {
  if [ -n "${docker_name}" ]; then
    echo "Cleaning up Docker container..."
    docker stop "${docker_name}" 2>/dev/null || true
    docker rm "${docker_name}" 2>/dev/null || true
  fi
}

# Register trap for EXIT, INT (Ctrl+C), TERM, and ERR
trap cleanup EXIT INT TERM ERR

if [ -z "$PREBUILT_BUILD_PATH" ]; then
    cd ../bottlecap
    arch=$(uname -a)
    # build bottlecap in debug mode
    if (echo $arch | grep -q "Darwin"); then
        PATH=/usr/bin:$PATH cargo zigbuild --target=aarch64-unknown-linux-gnu
        build_path=../bottlecap/target/aarch64-unknown-linux-gnu/debug/bottlecap
    else
        cargo build
        build_path=../bottlecap/target/debug/bottlecap
    fi
    cd -

else
    echo "using a prebuilt bottlecap from $PREBUILT_BUILD_PATH"
    build_path="$PREBUILT_BUILD_PATH"
fi

# run a hello world function in Lambda RIE (https://github.com/aws/aws-lambda-runtime-interface-emulator)
# the lambda_extension binary is copied to /opt/extensions
docker_name=$(docker create \
  --publish 9000:8080 \
  -e DD_API_KEY=XXX \
  -e DD_SERVERLESS_FLUSH_STRATEGY='periodically,1' \
  -e DD_LOG_LEVEL=debug \
  -e RUST_BACKTRACE=full \
  -e DD_ENV=dev \
  -e DD_VERSION=1 \
  "public.ecr.aws/lambda/nodejs:20" "index.handler")
echo -e 'export const handler = async () => {\n\tconsole.log("Hello world!");\n};' > /tmp/index.mjs
docker cp "/tmp/index.mjs" "${docker_name}:/var/task/index.mjs"
docker start "${docker_name}"           
docker exec "${docker_name}" mkdir -p /opt/extensions
docker cp "${build_path}" "${docker_name}:/opt/extensions/datadog-agent"
curl -XPOST "http://localhost:9000/2015-03-31/functions/function/invocations" -d '{}'
docker logs "${docker_name}"                                             
