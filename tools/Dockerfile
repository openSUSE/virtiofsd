# Match the Alpine build environment and static libraries used by .gitlab-ci.yml.
FROM rust:alpine

RUN apk add --no-cache bash libcap-ng-static libseccomp-static musl-dev \
    && rustup component add clippy rustfmt

ENV RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=yes" \
    LIBSECCOMP_LINK_TYPE=static \
    LIBSECCOMP_LIB_PATH=/usr/lib \
    LIBCAPNG_LINK_TYPE=static \
    LIBCAPNG_LIB_PATH=/usr/lib

COPY dev-container-entrypoint.sh /usr/local/bin/virtiofsd-dev-entrypoint
ENTRYPOINT ["virtiofsd-dev-entrypoint"]

WORKDIR /workspace
