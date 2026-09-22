# thogamm-model

Canonical integer-only ThogAMM schema-6 quote engine shared by the polling and
event SDKs. Quotes operate on validated immutable snapshots and perform no I/O.

Use either client through the [repository README](../README.md), or depend on
this crate directly from the same pinned Git tag. See [quoting](../docs/QUOTING.md)
for exact-input/output, marginal-price, limit and execution-context semantics.
`src/abi.rs` contains the supported contract tuple, events and settlement binding.
The `rpc` module is an explicit transport helper; quote calls never invoke it.

Licensed under [MIT](../LICENSE).
