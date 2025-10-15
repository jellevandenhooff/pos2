A (hopefully) lightweight replicated sqlite vfs using sqlite-plugin and the sqlite write-ahead log.

Based on graft-sqlite in https://github.com/orbitinghail/graft.

# References

Many ways to try and replicate sqlite. Some non-exhaustive list of projects I found:

- https://github.com/benbjohnson/litestream. WAL based, external process asynchronously reading WAL. Written in Go.
- https://github.com/superfly/litefs, WAL or rollback based (unsure), fuse filesystem replicating writes. Maybe asynchronous? Unsure. Written in Go.
- https://github.com/canonical/dqlite, WAL based, VFS filesystem synchronously replicated with raft. Written in C.
- https://github.com/orbitinghail/graft, rollback based, VFS filesystem synchronously replicated (I think) with a fancy backend. Written in Rust.

# License

Licensed under either of

Apache License, Version 2.0 (../sqlite-plugin/LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0)
MIT license (../sqlite-plugin/LICENSE-MIT or https://opensource.org/licenses/MIT)
at your option.

