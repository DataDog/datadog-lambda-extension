#!/bin/bash

set -e
arch=$(uname -a)
cd ../bottlecap
# build bottlecap in debug mode
if (echo $arch | grep -q "Darwin"); then
    PATH=/usr/bin:$PATH cargo zigbuild --target=aarch64-unknown-linux-gnu
    build_path=../bottlecap/target/aarch64-unknown-linux-gnu/debug/bottlecap
else
    cargo build
    build_path=../bottlecap/target/debug/bottlecap
fi
cd -

# run a hello world function in Lambda RIE (https://github.com/aws/aws-lambda-runtime-interface-emulator)
# the lambda_extension binary is copied to /opt/extensions
docker_name=$(docker create \
  --publish 9000:8080 \
  -e DD_API_KEY=XXX \
  "public.ecr.aws/lambda/nodejs:20" "index.handler")
echo -e 'export const handler = async () => {\n\tconsole.log("Hello world!");\n};' > /tmp/index.mjs
docker cp "/tmp/index.mjs" "${docker_name}:/var/task/index.mjs"
docker start "${docker_name}"           
docker exec "${docker_name}" mkdir -p /opt/extensions
docker cp "${build_path}" "${docker_name}:/opt/extensions/datadog-agent"
curl -XPOST "http://localhost:9000/2015-03-31/functions/function/invocations" -d '{}'
docker logs "${docker_name}"                                             
docker stop "${docker_name}" 
docker rm "${docker_name}" 
