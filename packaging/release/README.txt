Kranz release archive
=====================

This archive contains the Kranz command-line executable and the MIT license.

Verify the archive against SHA256SUMS from the same GitHub release before
extracting it. Then place `kranz` (Linux) or `kranz.exe` (Windows) somewhere on
your PATH and run:

    kranz --version
    kranz --help

The Linux x86-64 executable is statically linked with musl. The Windows x86-64
executable targets the MSVC runtime. macOS users should install the supported
Cargo package (`cargo install kranz --locked`) until signed and notarized macOS
archives are published.

Documentation: https://github.com/craigcode/kranz
