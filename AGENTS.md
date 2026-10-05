# Repository invariants

- The Ledger is the source of truth. Every actionable item is an Issue;
  conversations are not task storage.
- Peer identity is durable and independent of model/provider. Keep vendor APIs,
  payloads, and SDK types outside domain code.
- Use a small Rust workspace: `ledger`, `runtime`, and the root CLI. Do not add a
  Swarm abstraction, speculative micro-crates, or platform features outside scope.
- Change work through controlled Ledger operations. State changes and structured
  append-only events belong in the same database transaction.
- Claims must remain atomic under PostgreSQL concurrency. Preserve advisory-lock
  ordering and `FOR UPDATE SKIP LOCKED` semantics when changing persistence.
- Parentage and dependencies are distinct. Only Completed satisfies a dependency;
  completing prerequisites must automatically reconsider waiting dependents.
- Questions and Approvals are ordinary Ledger issues resolved by humans.
- Preserve useful repository tooling and CI. Mise pins tools; tasks live in
  `.mise/config.toml`.
- Verify with `cargo fmt --all`, workspace Clippy, and workspace tests. PostgreSQL
  tests must use a disposable server with a role allowed to create databases;
  never point integration tests at a user's real resources. Database test binary
  names begin with `integration` for the Nextest CI split.
- Keep changes on a task branch. Do not commit, push, or merge without instruction.
- Large, well-specified work may be delegated; independently verify shared results.

Read `docs/architecture.md` before changing lifecycle or scheduling semantics.
