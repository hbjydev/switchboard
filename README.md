# Switchboard

[![CI](https://github.com/hbjydev/switchboard/actions/workflows/ci.yaml/badge.svg)](https://github.com/hbjydev/switchboard/actions/workflows/ci.yaml)

A persistent AI harness.

The current foundation lives in `switchboard-kernel`, with distinct UUID types
in `switchboard-uuids`. Peer creation and renaming reject blank names; message
construction rejects blank text and preserves valid content unchanged. Aliases
use the complete `(provider, scope, external_id)` tuple as their identity.
Conversation membership preserves insertion order and is idempotent: adding an
existing participant keeps their role. Members and moderators can send;
observers cannot.

Event envelopes provide root and reply constructors. Roots correlate to their
own ID; replies inherit correlation and identify the immediate triggering event
as their cause. The application layer must create message-created events only
after a successful append, retain the message and its envelope together, and
validate peer existence, membership, addressed recipients, and cross-peer alias
uniqueness. Ordered history will use repository append order rather than time.
The agent runtime, repositories, and local conversation demo are still pending.

The toolchain is pinned in `rust-toolchain.toml`. Validate with:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Currently, workspace Clippy rejects the placeholder `assert!(true)` in
`crates/main/tests/dummy.rs`. The core crates can be checked independently with
`cargo clippy -p switchboard-kernel -p switchboard-uuids --all-targets -- -D warnings`.
