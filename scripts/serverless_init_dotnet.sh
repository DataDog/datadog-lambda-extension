#!/bin/sh

# Check if Alpine
if [ -f "/etc/alpine-release" ]; then
    echo "Alpine images are not supported for .NET automatic instrumentation with serverless-init"
    exit 1
fi

# Make sure curl is installed
apt-get update && apt-get install -y curl

# Get latest released version of dd-trace-dotnet
TRACER_VERSION=$(curl -s https://api.github.com/repos/DataDog/dd-trace-dotnet/releases/latest | jq -r '.tag_name' | cut -d '"' -f 4 | cut -c2-)

# Download the tracer to the dd_tracer folder
echo Downloading version "${TRACER_VERSION}" of the .NET tracer into /tmp/datadog-dotnet-apm.tar.gz
curl -L "https://github.com/DataDog/dd-trace-dotnet/releases/download/v${TRACER_VERSION}/datadog-dotnet-apm-${TRACER_VERSION}.tar.gz" -o /tmp/datadog-dotnet-apm.tar.gz

# Unarchive the tracer and remove the tmp
mkdir -p /dd_tracer/dotnet
tar -xzf /tmp/datadog-dotnet-apm.tar.gz -C /dd_tracer/dotnet
rm /tmp/datadog-dotnet-apm.tar.gz

# Create Log path
/dd_tracer/dotnet/createLogPath.sh
