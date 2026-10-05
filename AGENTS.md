# Repository invariants

- Switchboard owns durable coordination; agent backends own ephemeral agent
  execution. AgentBackend runs an autonomous attempt, not an LLM provider call.
- The Ledger is the source of truth. Every actionable item is an Issue;
  conversations are not task storage.
- Peer identity is durable and independent of model/provider. Keep vendor APIs,
  payloads, and SDK types outside domain code.
- External clients interact with the Ledger through the Switchboard control
  plane API, not by accessing PostgreSQL directly. Legacy direct CLI mode and
  infrastructure commands are transitional/operator exceptions.
- The persistent scheduler is orchestration; AgentBackend is execution. Keep
  leases, fencing, recovery, and durable state in the control plane.
- Use a small Rust workspace: `ledger`, `runtime`, `control-plane`, and the root
  CLI. Do not add a Swarm abstraction, speculative micro-crates, or platform
  features outside scope.
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

## Persistent control plane

- `switchboard serve` owns the HTTP API and normal autonomous scheduler. Reuse
  Worker execution semantics; PostgreSQL claims/fencing remain the correctness
  boundary for concurrent scheduler tasks and separate processes.
- Keep explicit HTTP request/response DTOs outside Ledger domain code. Mutations
  invoke controlled application/Ledger operations; expose neither arbitrary CRUD
  nor status assignment. Return structured, sanitized errors.
- Migrations are an explicit `switchboard migrate` deployment step before serve.
  Startup checks required database/schema access; health/readiness never migrate.
- API-only mode must start without Pi configuration. Preserve selectable fake and
  local Pi backends and durable Agent identity independent of backend choice.
- On shutdown stop accepting requests and claiming work, cancel active backends,
  bound cleanup, and stop heartbeats. Interrupted attempts remain recoverable by
  lease expiry; never apply a completion after shutdown cancellation.
- Authenticate every API route with provider-neutral issuer-issued JWT access
  tokens; only health/readiness probes are anonymous. Validate JWKS signature,
  issuer, resource audience, expiry, and not-before. Do not accept ID tokens,
  shared static API tokens, or an anonymous/trusted-network API mode.
- Require distinct user and workload API resource audiences, separate from CLI
  client IDs. Provider configuration must restrict user audiences to human login
  and workload audiences to client credentials; audiences alone do not prove the
  OAuth grant type. Human writes require `switchboard:write`, reads require
  `switchboard:read`, and current workload tokens are read-only.
- Derive HTTP mutation actors from verified issuer/subject mappings to durable
  Peers, never caller-supplied actor names. Keep identity mapping in the Ledger;
  do not tie authenticated identity or Agent identity to execution backends.
- Preserve browser CLI authorization-code login with PKCE S256, state/nonce
  checks, private credential caching and refresh, and runtime-only client secrets
  for workload client credentials. Send access tokens, not ID tokens, to the API.
- Normal issuer endpoints require HTTPS. Explicit literal-loopback HTTP is for
  local fixtures/development and must perform all authentication checks.
- Never log tokens, client secrets, raw claims, request bodies, database connection
  details, credentials, or internal error chains. Authentication tests use local
  scripted issuers and must not require an external provider account.
- Recommend one server replica initially. Atomic claims support concurrent
  orchestration, but do not claim fully tested HA or distributed leader election.
- No KubernetesEnvironment, gVisor execution, or Phoebe deployment manifests in
  this milestone. The existing non-root image runs serve without extra packages.
