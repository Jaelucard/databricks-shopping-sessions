FROM rust:1.95-slim-bookworm@sha256:d7482085ff5b415f84dba5647ae71606650bdef00db7aeb69f4b3d170c3e4082 AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked

# Keep the scorer's Debian ELF runtime and the three utilities used by the
# fixture cleanup and resource-check scripts. ldd resolves paths on amd64 and
# on arm64 for local native builds.
FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241 AS runtime
COPY --from=build /build/target/release/shopping-sessions /usr/local/bin/shopping-sessions
RUN set -eu; \
    mkdir -p /runtime/bin /runtime/usr/local/bin /runtime/usr/share; \
    cp /usr/local/bin/shopping-sessions /runtime/usr/local/bin/; \
    cp -L /bin/dash /runtime/bin/sh; \
    cp -L /bin/cat /bin/chmod /runtime/bin/; \
    for program in /usr/local/bin/shopping-sessions /bin/dash /bin/cat /bin/chmod; do \
        ldd "$program" >> /tmp/runtime-dependencies; \
    done; \
    awk '$2 == "=>" && $3 ~ /^\// { print $3 } $1 ~ /^\// { print $1 }' /tmp/runtime-dependencies \
        | sort -u > /tmp/runtime-paths; \
    while read -r library; do cp -L --parents "$library" /runtime/; done < /tmp/runtime-paths; \
    for package in libc6 libgcc-s1 dash coreutils; do \
        cp -L --parents "/usr/share/doc/$package/copyright" /runtime/; \
    done; \
    cp -a /usr/share/common-licenses /runtime/usr/share/

FROM scratch
COPY --from=runtime /runtime/ /
ENV PATH=/usr/local/bin:/bin
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/shopping-sessions"]
