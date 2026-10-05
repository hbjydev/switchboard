# Repository invariants

- Switchboard owns durable coordination; agent backends own ephemeral agent
  execution. AgentBackend runs an autonomous attempt, not an LLM provider call.
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
- Worker mutations must be fenced by the active, unexpired ExecutionAttempt ID,
  never only by PeerId. Preserve one active attempt per Issue, lock ordering,
  transactional outcome application, and attempt history through recovery.
- Use database time for lease authority. Heartbeats must stop with execution;
  recovery clears ownership and rechecks dependencies. Side-effect idempotency
  will use `(attempt_id, operation identity)`; Peer identity is not an attempt.
- Parentage and dependencies are distinct. Only Completed satisfies a dependency;
  completing prerequisites must automatically reconsider waiting dependents.
- Questions and Approvals are ordinary Ledger issues resolved by humans.
- Preserve useful repository tooling and CI. Mise pins tools; tasks live in
  `.mise/config.toml`.
- Verify with `cargo fmt --all`, workspace Clippy, and workspace tests. PostgreSQL
  tests use testcontainers with an isolated PostgreSQL container per test and
  must not read `DATABASE_URL` or target a user's real resources. Docker is required
  only for integration test execution. Database test binary
  names begin with `integration` for the Nextest CI split.
- Keep changes on a task branch. Do not commit, push, or merge without instruction.
- Large, well-specified work may be delegated; independently verify shared results.

Read `docs/architecture.md` before changing lifecycle or scheduling semantics.

## Agent backends and environments

- Keep Pi RPC handling in PiBackend; Worker only assembles durable value snapshots
  and applies fenced results. Never pass Ledger/database handles to backends.
- Each Pi attempt/reclaim uses a fresh `pi --mode rpc --no-session` process in a
  validated configured workspace. Pi session files are never authoritative state.
- Follow current Pi RPC documentation. Correlate prompt responses; consume stdout
  continuously and wait for `agent_settled`, not prompt acceptance or `agent_end`.
  Stderr is diagnostic only. Never log prompts, credentials, raw conversations,
  model thinking, or unsanitized diagnostics.
- Preserve cancellation and process ownership. Lease loss cancels execution,
  requests abort, and prevents result application. Cleanup must close input, bound
  waits, terminate if necessary, and reap children, including when futures drop.
  Local Unix processes use dedicated groups to terminate remaining group members.
- Keep process spawn/I/O/termination behind ExecutionEnvironment/RpcProcess.
  Kubernetes with gVisor/Agent Sandbox is future intent, not implemented support.
- Pi owns provider selection, tools, context management, retry, and compaction.
  Do not add native model APIs, Ledger mutation tools, delegation, or human-question
  extensions during this milestone. Keep FakeExecutor's deterministic demo working.
- RPC tests use the `rpc-fixture` all-feature scripted Rust child. No test may
  require Pi, provider credentials, or a real model. Retain the `integration*`
  database-test naming convention and isolated testcontainers fixtures.
