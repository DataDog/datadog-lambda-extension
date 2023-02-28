#!/bin/bash

# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache License Version 2.0.
# This product includes software developed at Datadog (https://www.datadoghq.com/).
# Copyright 2020 Datadog, Inc.

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

function build_prod() {
    BUILD_TAGS="$BUILD_TAGS" ARCHITECTURE=amd64 VERSION=1 \
        ./scripts/build_binary_and_layer_dockerized.sh
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
    size=$(stat -c "%s" "$BINRAY_DIR/datadog_extension-amd64/extensions/datadog-agent")
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

function aggregate_sizes() {
    python3 -c '
import sys, urllib.parse
packages = {}
for line in sys.stdin.readlines():
    pkg = line.strip().split()
    if len(pkg) != 4:
        continue
    _, size, _, name = pkg
    size = int(size)
    namesplit = name.split("/")
    path = "/".join(namesplit[:-1])
    package = namesplit[-1].split(".")[0]
    newname = f"{path}/{package}" if path else package
    packages[newname] = packages.get(newname, 0) + size
for pkg, size in sorted(packages.items(), key=lambda a: a[1], reverse=True):
    print(size,"\t",urllib.parse.unquote(pkg))'
}

function print_usage() {
    echo "visualize_size.sh: tools for debugging and visualizing binary sizes
    usage: ./scripts/visualize_size.sh TOOL_NAME

    Where TOOL_NAME is one of the following:
        list_symbols    prints a list of all dependencies of the extension
                        and the aggregated size of each

        size            prints the size of the extension binary in bytes as
                        built for production

        package_size    prints the size in bytes of all symbols that match
                        PACKAGE, requires that PACKAGE be given

        graph           save a a dependency graph to .layers/graph.svg. Shows
                        all paths to package PACKAGE, requires that PACKAGE be
                        given

        go_binsize_viz  view relative sizes of packages visually in the browser

    These additional variables are available:
        PACKAGE         the name of the package to debug, requires full package
                        name as would be used in an import statement

        CMD_PATH        the path to the entrypoint of the binary to be
                        visualized, defaults to cmd/serverless

        BUILD_TAGS      go build tags to use when building the extension,
                        defaults to serverless

    Tools required for drawing graphs:
        digraph         https://pkg.go.dev/golang.org/x/tools/cmd/digraph
        graphviz        https://graphviz.org/download/
    "
}

subcommand=$1
case $subcommand in
    list_symbols)
        build_with_symbols
        list_symbols | aggregate_sizes
        ;;

    size)
        build_prod
        binary_size
        ;;

    package_size)
        build_with_symbols
        package_size
        ;;

    graph)
        graph_package
        ;;

    go_binsize_viz)
        go_binsize_viz
        ;;

    *)
        print_usage
        exit 1
        ;;
esac
