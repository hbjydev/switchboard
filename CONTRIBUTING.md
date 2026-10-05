# Contributing

Read [AGENTS.md](AGENTS.md) and [the architecture note](docs/architecture.md).
Keep changes focused on a coherent Ledger/runtime behavior. Explain lifecycle,
concurrency, or storage changes and include meaningful PostgreSQL integration
tests when those semantics change.

Install the pinned tools with Mise and start Docker, then run:

```bash
mise run check
mise run complexity-check
```

`check` includes formatting, Clippy, unit and PostgreSQL integration tests,
documentation, and dependency policy checks. Database test binaries must start
with `integration` so the retained CI/Nextest split runs them against PostgreSQL.
Testcontainers starts and removes an isolated PostgreSQL 17 container per test,
using dynamic ports and the real application migrations. Tests ignore
`DATABASE_URL`; no manually provisioned database is needed. Unit tests need no
Docker. Never replace this isolation with personal, shared, or production resources.

Use Conventional Commit titles when preparing commits. Preserve the existing
release, CI, and tool configuration unless the implementation requires a change.
