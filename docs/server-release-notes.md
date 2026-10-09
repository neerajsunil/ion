Ion's native remote server for Linux x86-64 and ARM64.

Ion selects the matching binary, verifies SHA-256, and uploads it through SSH.
The server is cached under `~/.ion/server` and uses SSH stdio for persistent
file operations, indexing, native text search and filesystem notifications.
No Python, root access, remote download utility or extra listening port is required.

The binaries are statically linked with musl. `manifest.json` records the client
version, protocol and artifact checksums; `SHA256SUMS` is also provided.
Git and project-specific compilers/tools are still supplied by the remote host.
