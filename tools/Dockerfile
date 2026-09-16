# Match the Alpine build environment and static libraries used by .gitlab-ci.yml.
FROM rust:alpine

RUN apk add --no-cache bash libcap-ng-static libseccomp-static musl-dev \
    && rustup component add clippy rustfmt

# Keep static-link flags off host-side procedural macros, including during Clippy.
ENV CARGO_BUILD_TARGET=x86_64-unknown-linux-musl \
    RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=yes" \
    LIBSECCOMP_LINK_TYPE=static \
    LIBSECCOMP_LIB_PATH=/usr/lib \
    LIBCAPNG_LINK_TYPE=static \
    LIBCAPNG_LIB_PATH=/usr/lib

WORKDIR /workspace
