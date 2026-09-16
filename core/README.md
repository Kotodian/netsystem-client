# Client transport core

This directory owns client-side shared-region implementation details.

`vendor/dlmalloc` is built as a private `ONLY_MSPACES` allocator by
`hammer-shmem`. It is used only with the Data Heap pointer read from the
server-owned API region. The build hides its symbols and does not install,
export, or select a process global allocator.
