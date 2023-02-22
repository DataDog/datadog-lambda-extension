#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2020 Datadog, Inc.

# Publish the datadog lambda layer across regions, using the AWS CLI
# Usage: VERSION=5 REGIONS=us-east-1 publish_layers.sh
# VERSION is required.
set -e -o pipefail

# Move into the root directory, so this script can be called from any directory
DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
EXTENSION_DIR="$DIR/.."
cd "$EXTENSION_DIR"

if [ -z "$BUILD_TAGS" ]; then
    BUILD_TAGS="serverless"
fi

if [ -z "$CMD_PATH" ]; then
    CMD_PATH="cmd/serverless"
fi

BINRAY_DIR="$EXTENSION_DIR/.layers"
BINARY_NAME="datadog-agent"
BINARY_FILE="$BINRAY_DIR/$BINARY_NAME"
rm -rf "$BINRAY_DIR"
mkdir -p "$BINRAY_DIR"

AGENT_DIR="$EXTENSION_DIR/../datadog-agent"

function build_with_symbols() {
    if [ "$BUILD_EXTENSION" != "false" ]; then
        cd "$AGENT_DIR/$CMD_PATH"
        go build -tags "$BUILD_TAGS" -o "$BINARY_NAME" || cd "$EXTENSION_DIR"
        mv "$BINARY_NAME" "$BINRAY_DIR"
        cd "$EXTENSION_DIR"
    fi
}

function go_binsize_viz() {
    cd .layers/datadog_extension-amd64/extensions/

    echo "Analyzing go binary"
    go tool nm -size datadog-agent | c++filt > ../scripts/go-binsize-viz/agent.txt
    cd ../scripts/go-binsize-viz/

    echo "Converting data to json"
    python3 tab2pydic.py agent.txt > out.py
    python3 simplify.py out.py > data.js

    echo "Open http://localhost:8000/treemap_v3.html"
    python3 -m http.server
}

function list_symbols() {
    go tool nm -size -sort size "${BINARY_FILE}"
}

function binary_size() {
    size=$(stat -c "%s"  "$BINARY_FILE")
    echo "size of binary is ${size}"
}

function package_size() {
    size=$(list_symbols | grep "$PACKAGE" | awk '{print $2}' | paste -sd+ | bc)
    echo "size of ${PACKAGE} is ${size}"
}

function graph_package() {
    cd "$AGENT_DIR"
    deps=$(go list -tags "$BUILD_TAGS" -f '{{ .ImportPath }} {{ join .Imports " " }}' -deps "./$CMD_PATH")
    graphed=$(echo "$deps" | digraph allpaths "github.com/DataDog/datadog-agent/$CMD_PATH" "$PACKAGE")
    echo "$graphed" | draw_digraph
    mv "output.svg" "$BINRAY_DIR"
    cd "$EXTENSION_DIR"
    echo "graph saved to .layers/output.svg"
}

function draw_digraph() {
    python3 -c '
import sys
print("digraph {")
for line in sys.stdin:
    print("\"", end="")
    print("\" -> \"".join(line.strip().split()), end="")
    print("\"")
print("}")' | dot -Tsvg -o "output.svg"
}

subcommand=$1
case $subcommand in
    list_symbols)
        build_with_symbols
        list_symbols
        ;;

    size)
        build_with_symbols
        binary_size
        ;;

    package_size)
        build_with_symbols
        package_size
        ;;

    graph)
        graph_package
        ;;

    *)
        echo "unknown subcommand: ${subcommand}"
        exit 1
        ;;
esac
