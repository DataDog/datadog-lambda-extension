docker build . --progress=plain -t datadog/build-tools
dockerId=$(docker create datadog/build-tools)
docker cp $dockerId:/build_tools.zip .
unzip build_tools.zip
rm build_tools.zip