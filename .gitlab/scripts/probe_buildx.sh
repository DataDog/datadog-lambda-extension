#!/bin/sh

set -eu

# Older clients may not support every inspection command.
probe() {
    printf '\n== %s ==\n' "$*"
    if "$@"; then
        return 0
    else
        status=$?
        printf 'Probe exited with status %s\n' "$status"
    fi
}

printf 'Runner architecture: '
uname -m
printf '\n== Required buildx version ==\n'
docker buildx version

probe docker version
probe docker info --format '{{json .ClientInfo.Plugins}}'

printf '\n== Docker CLI path ==\n'
command -v docker

# Inspect metadata only, never Docker config.json or credential helpers.
for directory in \
    "${DOCKER_CONFIG:-$HOME/.docker}/cli-plugins" \
    /usr/local/lib/docker/cli-plugins \
    /usr/local/libexec/docker/cli-plugins \
    /usr/lib/docker/cli-plugins \
    /usr/libexec/docker/cli-plugins; do
    plugin="$directory/docker-buildx"
    if [ -e "$plugin" ]; then
        probe ls -l "$plugin"
        probe readlink -f "$plugin"
        probe sha256sum "$plugin"
    fi
done

# Inspect the existing builder only, do not create or bootstrap a worker.
probe docker buildx ls
probe docker buildx inspect
