Kranz release archive
=====================

This archive contains the Kranz executable, its MIT license, Rust dependency
and dashboard notices, and the build toolchain's Rust library copyright
inventory. Keep these files together when redistributing the archive.

Verify the archive against SHA256SUMS from the same GitHub release before
extracting it. Then place `kranz` (Linux/macOS) or `kranz.exe` (Windows) on
your PATH and run:

    kranz --version
    kranz --help
    kranz licenses

Linux x86-64 uses the GNU target and requires a compatible glibc host.
macOS has Apple silicon and Intel builds; these archives are not signed or
notarized application bundles. Windows x86-64 and ARM64 use the MSVC targets.
Download the archive matching your operating system and processor.

Rust dependency notices cover the union of all five targets and include
build dependencies. The Rust library inventory may include code not linked
into this executable. Dashboard notices are also served by kranz serve at
/THIRD_PARTY_NOTICES.txt. An SBOM is supplied separately in the release.

Documentation: https://github.com/craigcode/kranz
