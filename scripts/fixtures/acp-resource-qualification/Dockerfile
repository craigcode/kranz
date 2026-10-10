# Local qualification candidate only. Both sources must already be installed.
# No registry publication, dependency download or vendor login in this build.
FROM sha256:5d0f56837d3b506013d47da6f3294cf90f24b4e4dbaf2079828d75522167b743 AS adapters
# Keep the complete Rust toolchain, including its system linker and C headers.
# Both pinned images use Debian bookworm ARM64.
FROM sha256:c932e0f511ff0c152807ecc8d8789849bc94f4a4fff0ebd22f2a00bfa196f4a0
COPY --from=adapters /opt /opt
COPY --from=adapters /usr/local/bin /usr/local/bin
COPY --from=adapters /usr/local/lib /usr/local/lib
RUN set -eu; set -- /usr/local/rustup/toolchains/*/bin/rustc; \
    test "$#" = 1; test -x "$1"; \
    ln -s "$1" /usr/local/bin/rustc; \
    ln -s "$(dirname "$1")/cargo" /usr/local/bin/cargo; \
    ln -s "$(dirname "$1")/rustdoc" /usr/local/bin/rustdoc; \
    ldconfig; /usr/local/bin/python3 --version; \
    /usr/local/bin/rustc --version; /usr/local/bin/cargo --version; \
    /usr/bin/cc --version; /usr/local/bin/node --version
