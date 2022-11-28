docker buildx build --platform linux/amd64 -t datadog/build-tools . --load
dockerId=$(docker create --platform linux/amd64 datadog/build-tools)
docker cp $dockerId:/build_tools.zip .
rm -rf bin
unzip build_tools.zip
rm build_tools.zip
