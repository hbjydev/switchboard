# Contributing

Read [AGENTS.md](AGENTS.md) and [the architecture note](docs/architecture.md).
Keep changes focused on a coherent Ledger/runtime behavior. Explain lifecycle,
concurrency, or storage changes and include meaningful PostgreSQL integration
tests when those semantics change.

Install the pinned tools with Mise. Start a disposable PostgreSQL instance using
the README instructions and set `DATABASE_URL`, then run:

```bash
mise run check
mise run complexity-check
```

`check` includes formatting, Clippy, unit and PostgreSQL integration tests,
documentation, and dependency policy checks. Database test binaries must start
with `integration` so the retained CI/Nextest split runs them against PostgreSQL.
SQLx tests create isolated databases and require a database-creation-capable role.
Never run integration tests against personal, shared, or production resources.

Use Conventional Commit titles when preparing commits. Preserve the existing
release, CI, and tool configuration unless the implementation requires a change.
