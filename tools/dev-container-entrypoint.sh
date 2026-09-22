#!/usr/bin/env bash
set -eu

# An explicit native target keeps static-link flags off host-side procedural macros.
CARGO_BUILD_TARGET=${CARGO_BUILD_TARGET:-$(rustc --print host-tuple)}
export CARGO_BUILD_TARGET

exec "$@"
