#!/usr/bin/env bash
set -eu

# Build and run the requested command with CI's dependencies, retaining only local build caches.
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
engine=${CONTAINER_ENGINE:-docker}
image=${VIRTIOFSD_DEV_IMAGE:-virtiofsd-dev}
run_args=()
if [[ ${engine##*/} == podman ]]; then
    run_args+=(--userns=keep-id)
fi

if [[ $# == 0 ]]; then
    set -- cargo test --locked --target x86_64-unknown-linux-musl
fi

"$engine" build -t "$image" -f "$repo_dir/tools/Dockerfile" "$repo_dir/tools"
exec "$engine" run --rm "${run_args[@]}" \
    --user "$(id -u):$(id -g)" \
    --mount "type=bind,source=$repo_dir,target=/workspace" \
    --env HOME=/tmp \
    --env CARGO_HOME=/workspace/target/container-cargo \
    --env CARGO_TARGET_DIR=/workspace/target/container-build \
    "$image" "$@"
