Ion's native remote server for Linux (x86-64, ARM64) and macOS (Apple
Silicon).

Ion selects the matching binary, verifies SHA-256, and uploads it through SSH.
The server is cached under `~/.ion/server` and uses SSH stdio for persistent
file operations, indexing, native text search and filesystem notifications.
No Python, root access, remote download utility or extra listening port is required.

The Linux binaries are statically linked with musl; the macOS ones use only
the system libraries. `manifest.json` records the client
version, protocol and artifact checksums; `SHA256SUMS` is also provided.
Git and project-specific compilers/tools are still supplied by the remote host.
