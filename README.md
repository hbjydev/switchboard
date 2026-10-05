# Switchboard

Switchboard is a small, provider-independent autonomous worker runtime built
around a shared work **Ledger**. It uses Rust, Tokio, PostgreSQL, and SQLx. Its
first vertical slice creates work, claims it safely, creates sub-work, waits for
human input, and resumes to completion using a deterministic executor. No LLM
provider, API key, or model installation is required.

Every actionable item is an Issue, including Tasks, Questions, and Approvals.
Issues carry state, identity, hierarchy, dependencies, and structured lifecycle
history. Human and agent Peers are durable identities, independent of providers.

## Prerequisites and database

Use [Mise](https://mise.jdx.dev/) to install the repository's pinned Rust and
quality tools:

```bash
mise trust
mise install
mise run rust-components
```

Start a disposable local PostgreSQL server:

```bash
docker run --name switchboard-postgres \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=switchboard \
  -p 127.0.0.1:5432:5432 \
  -d postgres:17
```

Wait until `docker exec switchboard-postgres pg_isready -U postgres` reports
ready, then configure the application and run the embedded migrations:

```bash
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/switchboard
cargo run -- migrate
```

`.env.example` contains the same development setting. Mise loads `.env`; direct
Cargo invocations use the exported environment. Use a new database: these
migrations replace the former Switchboard application's schema and do not provide
an upgrade path for its data.

## Run the complete lifecycle

Create the demo issue, then drain available work:

```bash
cargo run -- issue create --title "Build demo feature" --demo
cargo run -- worker run --until-idle
cargo run -- issue list
```

The fake executor creates a child Task and blocks the parent on it. The worker
completes the child, automatically making the parent Ready. On its next execution,
the parent creates a human Question and waits. `--until-idle` exits once no work
is runnable; a pending human answer is expected at this point.

Copy the Question's UUID from `issue list` and answer it:

```bash
cargo run -- issue answer QUESTION_UUID --answer "Use option A"
cargo run -- worker run --until-idle
cargo run -- issue show PARENT_UUID
cargo run -- issue events PARENT_UUID
```

Replace `QUESTION_UUID` and `PARENT_UUID` with the actual IDs printed by the CLI.
The answer completes the Question, unblocks the parent, and lets the final worker
run complete it. Restarting the worker between steps preserves progress because
the Ledger stores the child work and answer.

To keep a worker polling instead, run `cargo run -- worker run`. It polls every
500 ms while idle. The default agent is `worker`; select another durable identity
with `worker run --agent another-worker`. The CLI's default human identity is
`operator`, configurable with the global `--human` option.

## Inspect and manage work

```bash
cargo run -- issue create --title "A simple task"
cargo run -- issue list --ready
cargo run -- issue show ISSUE_UUID
cargo run -- issue events ISSUE_UUID
cargo run -- issue create --title "Later work" --backlog
cargo run -- issue ready ISSUE_UUID
cargo run -- issue depend ISSUE_UUID DEPENDENCY_UUID
cargo run -- issue complete ISSUE_UUID
cargo run -- issue cancel ISSUE_UUID --reason "Superseded"
cargo run -- --help
```

Ordinary Tasks complete immediately in the fake executor. `--demo` selects the
persisted child/question lifecycle by setting the description to `demo`; a
description beginning with `fail:` produces an explainable execution failure.
These are fixture conventions, not a prompt language. `issue create` also accepts
`--kind question|approval`, `--description`, and `--priority` (higher runs first).
Use `issue answer` to resolve human issues. Manual task completion applies to
Ready work; worker-owned execution uses claimant-checked Ledger operations.

## Workspace

```text
crates/ledger/    Domain types, controlled operations, PostgreSQL persistence
crates/runtime/   Agent, worker loop, executor contract, deterministic executor
src/main.rs      CLI and wiring
migrations/      PostgreSQL schema
docs/architecture.md
```

## Development and verification

With Docker running (no `DATABASE_URL` or manually started PostgreSQL required):

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
mise run check
mise run complexity-check
```

Integration tests use testcontainers to start a separate PostgreSQL 17 container
per test, apply the application migrations, and remove the container when the test
finishes. Ports are allocated dynamically; tests never read `DATABASE_URL`. Docker
must be accessible, and the first run pulls `postgres:17` if needed. No external
database or database-creation role is required. `mise run test` runs unit tests
without Docker; `mise run test-int` runs container-backed database tests. CI retains
that Nextest split and uses the runner’s Docker daemon. Integration coverage
includes concurrent claiming, dependency cycles and unblocking, human resolution,
ownership, and event history.

The repository retains pinned tooling, Nextest archives, formatting and Clippy
policy, dependency auditing, Renovate, release automation, signed release assets,
and a nonroot container build. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Current limits

- The executor is deterministic. There are no provider integrations or software
  editing tools.
- Claims have no leases or automatic crash recovery. Interrupted work can remain
  Claimed or Running; inspect history, cancel it, and create replacement work.
- Dependency and lifecycle mutations use a coarse transaction advisory lock.
  Concurrent claims remain safe; write throughput is intentionally modest.
- Only Completed satisfies dependencies. Failed or Cancelled prerequisites leave
  dependent work blocked for explicit intervention.
- Approval resolution records decision text without enforcing approve/reject
  policy. Peer identity is not authentication; the CLI assumes trusted callers.
- No HTTP frontend, chat integration, distributed scheduler, Swarm, memory system,
  automatic retry, or elaborate permissions are implemented.

See [the architecture note](docs/architecture.md) for state semantics, transaction
boundaries, and open design questions. The next useful work is claim recovery,
execution idempotency, then a narrowly scoped real executor behind the existing
provider-independent boundary.
