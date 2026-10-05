# Architecture

## Work lives in the Ledger

An Issue is the unit of actionable work: a Task, Question, or Approval. The
Ledger stores current state, dependencies, parentage, and an append-only event
history in PostgreSQL. Conversation history is never authoritative task storage.
The current-state tables are authoritative; events explain changes without
requiring event replay or introducing full event sourcing.

A Peer is a durable human or agent identity. It has no model or provider. The
runtime's Agent and AgentBackend describe who performs work and how an autonomous
execution runs. Model/provider selection belongs to a backend's harness, outside
the domain and durable Peer identity.
An ExecutionAttempt is one claim of one Issue by a Peer. A Peer may execute the
same Issue repeatedly; every claim gets a new AttemptId. The Issue remains the
authoritative business state, while attempt rows explain individual executions.
`Issue.current_attempt_id` points only to a currently active attempt.

## Components

- `crates/ledger`: typed UUID identities, domain types, controlled lifecycle
  operations, migrations, and SQLx persistence.
- `crates/runtime`: agent definition, Worker, AgentBackend request/result contract,
  Pi RPC adapter, ExecutionEnvironment transport, and deterministic fake executor.
- `crates/control-plane`: small application layer, versioned HTTP API, stable
  DTOs, authentication, and persistent scheduler lifecycle.
- `src/main.rs`: Clap CLI, configuration, remote API client, and server wiring.
- `migrations`: clean PostgreSQL schema for this application.

There is deliberately no Swarm object. Multiple workers coordinate by claiming
ordinary Ledger work. A scheduling hierarchy is unnecessary for this milestone.

## Persistent control plane and deployment

`switchboard serve` is the persistent control plane. It owns the Ledger handle,
HTTP API, recovery, scheduling, and execution control. PostgreSQL remains the
durable source of truth. The persistent scheduler is orchestration; AgentBackend
is execution. HTTP types and transport errors stay outside Ledger domain code.
A small application layer shares issue operations between HTTP handlers and the
transitional direct CLI instead of duplicating lifecycle rules.
External clients interact with the Ledger through the Switchboard control plane
API, not by accessing PostgreSQL directly.

The initial production topology is:

```text
clients (CLI, future UI/integrations)
               |
       Switchboard Deployment (1 replica initially)
       + HTTP API
       + scheduler / recovery
       + execution controller ----> AgentBackend / local execution
               |
           PostgreSQL

Later: execution controller -> KubernetesEnvironment -> gVisor agent Pods
```

Future execution isolation does not move durable orchestration into Pi or pods.
KubernetesEnvironment and gVisor execution are not implemented. Actual Phoebe
configuration belongs in `hbjydev/phoebe`, not this repository.

Deployment order is `switchboard migrate`, OIDC issuer/client/resource
configuration, then `switchboard serve`. Serve never applies migrations; startup
validates database/schema and required identity-provider configuration before
serving. Authentication is required for API-only and local servers too. PostgreSQL needs persistent storage, backup, and
ordinary database operations. Container restarts must reuse that database. The
existing non-root image accepts `serve` through its entrypoint and requires no
extra packages for the API or fake backend. Pi local execution still needs an
operator-provided Pi installation, credentials, and workspace; secrets are runtime
configuration and must not be baked into an image.

The server listens on `0.0.0.0:8080` by default. `DATABASE_URL` is required only by
serve, migrations, and direct database/debugging commands. Configure listen address,
lease duration, scheduler interval/concurrency, backend, and agent profile through
CLI/environment settings documented in the README. `--scheduler=false` runs just
the API and does not require Pi configuration. One configured Agent Peer serves
all scheduler slots in this milestone; capability matching is deferred.

A bounded set of Tokio worker tasks uses the existing Worker loop. Each slot
recovers expired attempts, atomically claims eligible work, runs an independent
attempt with its own heartbeat and cancellation lifecycle, then applies the fenced
result. Successful work is followed by another tick immediately; idle/error ticks
wait for the configured modest polling interval (default one second). There is
no in-memory durable queue. Claims and fencing protect against duplicate ownership
across slots or server processes. Start with one replica; concurrent Ledger claims
are safe, but full server HA lifecycle behavior is not claimed.

Ctrl-C and SIGTERM stop HTTP acceptance and new claims, cancel active executions,
and allow bounded backend cleanup. Heartbeat renewal stops when cancellation
takes effect. Shutdown cancellation is
never a successful completion: unfinished attempts retain their leases and become
recoverable after expiry. Restarted servers run the same recovery path, preserve
attempt history, and create a fresh AttemptId when reclaiming. This mechanism also
handles abrupt process crashes; it cannot undo external effects.

## HTTP contract and operator boundary

The JSON API uses UUID identities and explicit boundary DTOs under `/api/v1`:

| Method | Route | Operation |
| --- | --- | --- |
| POST / GET | `/api/v1/issues` | Create / list Issues. |
| GET | `/api/v1/issues/:id` | Inspect an Issue. |
| GET | `/api/v1/issues/:id/events` | Inspect durable lifecycle history. |
| GET | `/api/v1/issues/:id/attempts` | Inspect execution attempts. |
| POST | `/api/v1/issues/:id/ready` | Make eligible backlog work available. |
| POST | `/api/v1/issues/:id/complete` | Human completion under lifecycle rules. |
| POST | `/api/v1/issues/:id/cancel` | Cancel with a reason. |
| POST | `/api/v1/issues/:id/answer` | Resolve a Question or Approval. |
| POST | `/api/v1/issues/:id/dependencies` | Add a prerequisite. |
| GET | `/api/v1/status` | Safe version, scheduler, and backend configuration. |
| GET | `/api/v1/me` | Verified identity and its durable Peer ID. |

This API exposes controlled use cases rather than database CRUD or arbitrary
status changes. Domain conflicts and invalid transitions return structured client
errors; unknown Issues return 404. Responses use
`{"error":{"code":"issue_not_found","message":"..."}}` for errors and do not
include SQL, connection information, credentials, or internal error chains.

`GET /healthz` reports process liveness. `GET /readyz` checks database/schema
availability and bounded issuer/JWKS availability needed to serve requests.
Fresh cached signing keys permit continued operation during a provider outage;
readiness fails when required dependencies are unavailable. Neither endpoint mutates state or runs migrations. Both are
unauthenticated for later Kubernetes probes. Request tracing includes method,
route, status, latency, and request ID; request bodies are not logged. Execution
tracing retains AttemptId. Status exposes no provider credentials or raw settings.

Every `/api/v1` endpoint requires `Authorization: Bearer <access-token>`.
Switchboard is a provider-neutral OIDC/OAuth2 resource server and does not issue
its own tokens. JWT signature validation uses discovered issuer JWKS; validation
also checks issuer, API resource audience, expiry, and not-before claims. Discovery
and key requests are bounded and keys are cached. Normal issuer endpoints require
HTTPS. `SWITCHBOARD_OIDC_ALLOW_LOOPBACK_HTTP` explicitly permits literal loopback
HTTP for fixtures/development, while retaining discovery and all token checks.
There is no anonymous or shared-static-token API mode. Missing/invalid credentials
return 401 and insufficient authority returns 403. Probes are the only anonymous
HTTP routes. Tokens, secrets, request bodies, and raw claims are never logged.
Opaque-token introspection is not implemented.

The server requires `SWITCHBOARD_OIDC_ISSUER` and
`SWITCHBOARD_OIDC_USER_AUDIENCE`. Optional
`SWITCHBOARD_OIDC_WORKLOAD_AUDIENCE` enables OAuth2 workloads. These are distinct
resource audiences, not the public CLI client ID. One configured issuer serves
both identities; it must restrict user-audience grants to human login and
workload-audience grants to client credentials. Audience separation alone does
not prove the original grant type: these restrictions are an explicit identity
provider configuration contract. Switchboard does not configure the provider.

`switchboard:read` permits API GET requests. Human mutations require a user token
with `switchboard:write`; current workload tokens can only read with
`switchboard:read`. Attempt-specific scopes and execution callbacks are not
implemented. Migration 0003 maps verified `(issuer, subject)` to a durable Peer;
mutation attribution uses that mapping rather than caller-supplied actor names.
HTTP DTOs reject `actor` fields. `/api/v1/me` exposes safe verified identity and
Peer ID, never credentials. Fine-grained ownership/RBAC remains outside scope.

The public CLI uses authorization code flow with PKCE S256. `auth login` opens a
browser and receives a loopback callback on `127.0.0.1:8400/callback` by default;
`--redirect-port` selects another port. Callback state and ID-token nonce are
validated, and only the issued access token is sent to the API. The CLI client
uses `SWITCHBOARD_OIDC_CLIENT_ID`, defaults to scopes
`openid profile offline_access switchboard:read switchboard:write`, and supports
`--resource` for RFC 8707 audience selection. `auth client-credentials` obtains
workload access tokens with runtime-only `SWITCHBOARD_OAUTH_CLIENT_ID` and
`SWITCHBOARD_OAUTH_CLIENT_SECRET`, defaulting to `switchboard:read`.

Credentials use a private JSON cache at
`$XDG_STATE_HOME/switchboard/credentials.json` (falling back to
`~/.local/state/switchboard/credentials.json`), overrideable with
`SWITCHBOARD_CREDENTIALS_FILE`. On Unix its mode is 0600. Cached user sessions refresh
automatically; `auth logout` only removes the local cache. Client secrets are not
persisted; expired workload tokens must be reissued using runtime credentials.
`SWITCHBOARD_ACCESS_TOKEN` supplies an already issued access token and
overrides the cache. Tests use a scripted local issuer without external provider
accounts or real models.

Issue CLI commands use the HTTP API when `SWITCHBOARD_URL` (or the corresponding
CLI option) is set and require no `DATABASE_URL`. Legacy direct database mode is
retained temporarily for compatibility; migrate and worker debugging commands
remain operator tools with direct database access. Future integrations must use
the control plane API.

A global SSE event stream is deferred. A durable, resumable cursor must respect
transaction ordering and bounded polling; adding a misleading best-effort cursor
would expand this milestone. Per-Issue event inspection is available now. A future
stream will expose high-level Ledger changes, never model tokens or reasoning,
and correctness will remain independent of whether any client consumes it.

## States

| State | Meaning |
| --- | --- |
| Backlog | Recorded, but not yet available to workers. |
| Ready | A worker may claim this Task now. |
| Claimed | An agent has atomically acquired this work. |
| Running | The claimant has started execution. |
| Blocked | Unfinished dependencies prevent execution. |
| WaitingForHuman | A human issue needs resolution, or work depends on one. |
| Completed | Successfully resolved; satisfies dependencies. |
| Failed | Execution failed, with a recorded explanation. |
| Cancelled | Work was explicitly abandoned. |

Domain operations validate transitions and active execution-attempt authority. They update state
and append structured events in the same transaction. Callers cannot assign an
arbitrary status through the Ledger API. Workers do not execute Questions or
Approvals.

## Claiming and transaction boundaries

Ready Tasks are ordered by descending priority, creation time, then ID.
`claim_next_issue` uses PostgreSQL row locks with `FOR UPDATE SKIP LOCKED` and
creates a leased attempt, updates the Issue's current attempt and owner, and appends
IssueClaimed/ExecutionClaimed events in one transaction. It returns a Claim containing
the Issue and attempt, including the lease expiry. Concurrent
workers cannot successfully claim the same issue. Direct claims enforce the
same eligibility and ownership rules.

For this initial implementation, graph and lifecycle mutations acquire an
exclusive transaction-scoped PostgreSQL advisory lock. Claim transactions acquire
the corresponding shared lock, allowing claims to overlap while preventing races
with dependency changes. This coarse lock makes cycle checks and automatic
unblocking straightforward and correct. It is a deliberate throughput tradeoff,
not a distributed scheduling framework. All mutations must go through controlled
Ledger operations to preserve these invariants. External clients interact with the Ledger
through the Switchboard control plane API, not by accessing PostgreSQL directly.

## Dependencies and hierarchy

An edge means the issue depends on another issue completing. Self-dependencies
and dependency cycles are rejected. Only Completed satisfies an edge: a Failed or
Cancelled prerequisite leaves dependents blocked for explicit intervention.
Completing a prerequisite recomputes affected dependents in the same transaction;
when every prerequisite is completed, eligible waiting work becomes Ready again.
State-change events make the reason inspectable.

Parentage expresses decomposition and is distinct from an edge. Creating a child
does not inherently block the parent. The Ledger supports atomic child creation
with an explicit blocking dependency so an executor can safely hand off sub-work.

## Human input

Questions and Approvals are ordinary issues awaiting resolution by a human peer.
They form an unassigned shared inbox in this milestone; an authenticated human
with `switchboard:write` can resolve them. A worker can create one and depend on it. The CLI records a nonempty human answer, completes
the human issue, and unblocks dependent work when all dependencies are satisfied.
Approvals currently carry decision text; there is no enforced approve/reject
policy. Applications requiring authorization semantics must add that policy
before treating an Approval as permission to perform a sensitive action.

## Execution attempts, leases, and fencing

Attempt states are Claimed, Running, Completed, Failed, Expired, and Cancelled.
Completed means the invocation's outcome was applied: the Issue may have completed
or handed off to blocking child work/human input. Resuming that Issue creates a new
attempt. Administrative cancellation or a new blocking dependency cancels the active
attempt. No active attempt remains attached to waiting or terminal work.

Migration 0002 adds execution_attempts and current_attempt_id without changing 0001.
A partial unique index permits at most one Claimed/Running attempt per Issue,
including attempts whose leases have elapsed but are not yet recovered. A composite
foreign key ensures the current attempt belongs to that Issue; an Issue check
requires a current attempt exactly when Claimed/Running. Attempt constraints require
finished_at exactly for terminal attempts. Existing Claimed/Running issues receive
an immediately expiring attempt and an adoption event, so upgrading permits recovery
without erasing prior Issue history.

Leases default to 30 seconds; Ledger handles accept 3 milliseconds–1 day via
with_lease_duration, and the CLI exposes --lease-seconds / SWITCHBOARD_LEASE_SECONDS.
PostgreSQL clock_timestamp() determines claim expiry, validation, renewal, and
recovery after lock acquisition. Worker timers only schedule renewals; they do not
determine authority. Ledger tests expire rows explicitly, rather than sleep and
assume a lease elapsed.

mark_running, heartbeat, complete_attempt, fail_attempt, and
create_child_for_attempt take AttemptId rather than PeerId. Each transaction locks
in this order: advisory lock, Issue row, attempt row. It validates the Issue's
current attempt, owner, active states, and unexpired lease using database time
before writing. A known Peer returning with an old attempt cannot renew or mutate
the Issue, including before recovery. IDs are not authentication credentials. The HTTP boundary authenticates the
caller before invoking domain operations. Human administration uses human-only operations without
requiring an attempt; the agent Peer-only mutation path cannot bypass fencing. Root work intake through
create_issue is human-only; execution-generated Tasks, Questions, and Approvals
use the fenced child/handoff operation.

Terminal outcome application is transactional. A child or human-input outcome
inserts the child, records the handoff, adds any blocking dependency, and ends the
attempt in the same transaction. A nonblocking child outcome completes the parent.
Replays return ExecutionLost before adding a child or contradictory terminal event;
there is deliberately no terminal-result cache. ExecutionClaimed, ExecutionStarted,
ExecutionCompleted/Failed/Cancelled/Expired and ExecutionRecovered explain attempts
alongside existing Issue events. All execution events include attempt_id. Heartbeats
update heartbeat_at and lease_expires_at on the attempt only, avoiding event noise.

## Worker heartbeats and recovery

The worker recovers expired work, claims, starts, loads durable context, executes,
then applies an outcome. ExecutionRequest includes AttemptId, Peer, instructions,
Issue, parent, persisted children, prerequisites, and workspace. AgentBackend is
independent of model providers; the legacy Executor adapter preserves the fake demo. An inline Tokio select renews at one
third of the lease duration while context loading and execute() are pending. The
first renewal is delayed and missed ticks use Delay. Cancellation takes priority
over heartbeat and result readiness; while active, renewal wins simultaneous
result readiness. Cancellation also interrupts database waits during recovery,
claiming, startup, and renewal. No detached heartbeat task exists: dropping the tick or a failed renewal
cancels execution and stops the timer. Renewal failure propagates; ExecutionLost
clearly identifies lost authority. Cancellation gets up to five seconds of cooperative
backend cleanup before the future is dropped. Applying an outcome still checks
authority, even if no heartbeat observed the loss. Process handles signal their
supervisor on drop, so cleanup continues while the Tokio runtime is alive. Cancellation
cannot undo external effects already performed.

recover_expired_attempts is explicit and called before each worker claim. It takes
the existing exclusive graph/lifecycle lock, locks expired Claimed/Running Issues,
marks their attempts Expired, clears current_attempt_id and owner, and readies the
Issue with a structured recovery event. It recomputes dependencies in the same
transaction. Claims/heartbeats take shared advisory locks; recovery never upgrades
a shared lock, so claims, renewals, and recovery cannot race their authority changes.
Recovery and claim are separate transactions: any worker may win the new claim.
The partial unique index remains a database backstop. worker recover exposes
maintenance without execution; normal polling workers and persistent scheduler
slots also perform recovery.

The deterministic executor's child/question lifecycle persists through worker
restarts. Claims and recoveries require no LLM, provider account, conversation
session, or external tool. The coarse locking strategy prioritizes correctness and
may delay renewals under heavy write contention; lease duration should leave headroom.

## Autonomous execution backends

**Switchboard owns durable coordination; agent backends own ephemeral agent execution.**

AgentBackend means “run this execution attempt with an autonomous agent harness,”
not “call a model provider.” Worker owns scheduling, leases, heartbeats, human
attention, and fenced outcome application. Backend requests contain value snapshots
and no Ledger/database handles. Backend results distinguish a textual completion,
explicit task failure, cancellation, and BackendError (transport/protocol failure).
Worker applies backend errors as fenced failures. Cancelled output is never applied.
The demo compatibility adapter retains existing fake child/human handoffs without
exposing those capabilities to Pi. Future backends can include OpenCode, native
models, and humans without changing Peer identity.

PiBackend starts one fresh process per attempt, including every retry/reclaim,
with `--mode rpc --no-session`. It sends a correlated JSONL `prompt` command and
continuously consumes stdout responses/events. Prompt acceptance and `agent_end`
are not completion; `agent_settled` is the current Pi session-level completion
boundary after retries, compaction, and queued activity. Events may precede the
acknowledgement; both acceptance and settlement are required. A `handled` prompt
without a run is rejected clearly. Unsupported interactive extension requests also
fail clearly. Older Pi versions without `agent_settled` time out rather than being
mistaken for successful execution.

Only text blocks from the last authoritative assistant `message_end` become the
final summary. Thinking, tools, streaming deltas, and raw conversations are neither
logged nor stored. Temporary error messages can be superseded by a successful Pi
retry. A settled error, exhausted retries, or output-limit stop is a failure;
`SWITCHBOARD_FAILED:` at the start of final text is the explicit task-failure convention.
Missing final text is a protocol error. Prompts are bounded to 32 KiB and summaries
to 8 KiB on UTF-8 boundaries. Each wire record is limited to 8 MiB before parsing.
A successful summary is added to IssueCompleted metadata in the existing fenced
completion transaction. No migration or authoritative Pi session storage is needed.

## Execution environment and process ownership

ExecutionEnvironment asynchronously creates an RpcProcess from a ProcessSpec: executable,
arguments, working directory, shutdown grace, and adapter-supplied abort record.
The transport exposes send, next_record, and finish; it does not interpret Pi RPC.
LocalProcessEnvironment validates cwd and uses Tokio process primitives. A
supervisor owns the child and stdin, a bounded reader consumes stdout, and stderr
is drained separately without parsing or logging its possibly sensitive contents.
PiBackend only knows the transport contract, so process execution can later move
to another environment without spreading Command usage through Worker.

Settlement closes stdin for orderly shutdown and awaits child exit before returning
an outcome. Cancellation requests abort, closes input, waits briefly, then kills
and reaps the direct child if needed. Writes and graceful waits have bounded
deadlines (two seconds by default); execution has a configurable deadline (one
hour by default). Dropping the handle triggers supervisor cancellation. Tokio
kill_on_drop and a synchronous Unix process-group guard are last resorts during
runtime teardown. Unix cleanup kills remaining group members too; processes that
escape the group are outside this local mechanism. Non-Unix cleanup only covers
the direct child. There are no detached heartbeat tasks.

Local cwd/resource discovery and Pi credentials use Pi's ordinary runtime behavior.
No workspace provisioning, credentials cloning, shell policy, or isolation is added.
Provider/model arguments are infrastructure choices; they never alter Peer identity.
CLI Ctrl-C waits for cancellation/cleanup, leaving interrupted work available for
normal lease recovery. Fencing stops stale Ledger updates, not already-performed
filesystem effects.

A future KubernetesEnvironment should own pod/container creation, streaming I/O,
termination, and isolated workspace provisioning. Kubernetes with gVisor/Agent
Sandbox is intended future work and is not implemented. The current ProcessSpec
and PathBuf workspace express local assumptions; remote execution will need
workspace/environment descriptors and a nonlocal exit/cleanup implementation.
Pi sessions will remain ephemeral there as well. Container/pod deletion should
replace Unix group cleanup and cover all descendants, including detached tools.

## Idempotency boundary

Side effects on behalf of an attempt should use `(attempt_id, operation identity)`
as their durable idempotency scope. The stable attempt ID is available to AgentBackend
implementations. A reclaimed execution has a different scope; avoiding duplicate
external effects across attempts additionally requires durable business-operation
identity/checkpoints or reconciliation. Ledger fencing prevents stale database
outcomes, not external effects already in flight. No external tool or generic
idempotency subsystem is introduced here.

## Open questions and future boundaries

1. Idempotent external effects, cancellation contracts, and durable checkpoints.
2. Retry budgets/backoff for repeated crashes and explicit failed work.
3. Richer human decisions, rejection, reassignment, and failed dependency handling.
4. Agent capability selection and fairness without coupling identity to providers.
5. Ledger delegation/human tools behind fenced backend operations, followed by
   isolated Kubernetes/gVisor execution environments.
6. Finer-grained graph concurrency if measured contention warrants it.

The migrations replace the old Switchboard schema. They are a clean start and do
not implement an in-place migration from the former application. Use a new
database; any deliberate deletion of an old database is a separate operator action.
