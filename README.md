# netsystem-client

`netsystem-client` owns the external client implementations for the Netsystem
Binary API and stats protocols. It is a standalone Git repository and is not a
submodule of the server repository.

The repository is language-neutral. Protocol schemas and version contracts are
the shared source of truth; language bindings live in independent directories
and must not change the server or protocol contract.

Planned layout:

```text
schema/          versioned, language-neutral protocol declarations
core/            private client transport core and shared-region allocator
rust/            Rust bindings and RPC services
go/              Go bindings
python/          Python bindings
```

Client connection lifecycle, request correlation, transport selection, and
shared-memory mapping belong here. Business operations such as
`VpeService::show_version()` belong to typed RPC service layers built on the
client, not to the connection object itself.
