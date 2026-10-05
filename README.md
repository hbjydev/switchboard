# Switchboard

Switchboard is a persistent control plane for autonomous work, built around a
shared PostgreSQL **Ledger**. Its HTTP API records work and human input, while its
persistent scheduler recovers interrupted executions, claims runnable work, and
executes it through an AgentBackend. It uses Rust, Tokio, Axum, and SQLx. The local
Pi backend runs Pi's normal coding-agent loop over RPC; the deterministic fake backend
retains the child-work and human-question demo without requiring a model provider.

Every actionable item is an Issue, including Tasks, Questions, and Approvals.
Issues carry state, identity, hierarchy, dependencies, and structured lifecycle
history. Human and agent Peers are durable identities, independent of providers. Each
claim creates a durable ExecutionAttempt with a finite lease and a unique fencing
ID, so a disappeared worker can be recovered without accepting its late result.

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

## Start the control plane

Run migrations first, configure your OIDC issuer and API audience, then start the
persistent service:

```bash
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/switchboard
cargo run -- migrate
export SWITCHBOARD_OIDC_ISSUER=https://identity.example.com
export SWITCHBOARD_OIDC_USER_AUDIENCE=https://switchboard.example.com/api/user
cargo run -- serve
```

Serve checks connectivity and the required schema at startup; it does not run
migrations. The default fake backend needs no Pi installation or model-provider
account. OIDC authentication is required even in API-only or local operation.
The server owns the API and normal autonomous worker loop, so a separate worker
process is unnecessary. For an API-only service use
`cargo run -- serve --scheduler=false`. Pi configuration is only validated when
Pi execution is enabled.

In another terminal, use the CLI as an HTTP client:

```bash
export SWITCHBOARD_URL=http://127.0.0.1:8080
export SWITCHBOARD_OIDC_ISSUER=https://identity.example.com
export SWITCHBOARD_OIDC_CLIENT_ID=switchboard-cli
export SWITCHBOARD_OIDC_USER_AUDIENCE=https://switchboard.example.com/api/user
curl "$SWITCHBOARD_URL/healthz"
cargo run -- auth login
cargo run -- issue create --title "A simple task"
cargo run -- issue list
```

These issue commands do not need `DATABASE_URL` in remote mode. Login opens the
browser, authenticates against your issuer, and stores credentials privately for
subsequent API requests. Without `SWITCHBOARD_URL`, issue commands retain legacy
direct database behavior;
use that only for transitional development/operator access. `migrate` and debugging
`worker` commands still use `DATABASE_URL`.

External clients interact with the Ledger through the Switchboard control plane
API, not by accessing PostgreSQL directly. The persistent scheduler is
orchestration; AgentBackend is execution.

| Environment | Default / requirement |
| --- | --- |
| `DATABASE_URL` | Required by serve and direct database commands. |
| `SWITCHBOARD_LISTEN_ADDRESS` | `0.0.0.0:8080` |
| `SWITCHBOARD_SCHEDULER` | `true`; set `false` for API-only mode. |
| `SWITCHBOARD_WORKER_CONCURRENCY` | `1` independent scheduler slots. |
| `SWITCHBOARD_SCHEDULER_INTERVAL_MS` | `1000` while idle or retrying a tick. |
| `SWITCHBOARD_SHUTDOWN_GRACE_SECONDS` | `10` for bounded draining/cleanup. |
| `SWITCHBOARD_LEASE_SECONDS` | `30` (1–86400 seconds). |
| `SWITCHBOARD_BACKEND` | `fake`; `pi-local` enables local Pi execution. |
| `SWITCHBOARD_AGENT_NAME` | `worker`, a durable Agent Peer. |
| `SWITCHBOARD_AGENT_INSTRUCTIONS` | Complete the assigned task and report its result. |
| `SWITCHBOARD_OIDC_ISSUER` | Required HTTPS issuer for server and CLI login. |
| `SWITCHBOARD_OIDC_USER_AUDIENCE` | Required user API resource audience for serve and browser login. |
| `SWITCHBOARD_OIDC_WORKLOAD_AUDIENCE` | Optional distinct workload API resource audience. |
| `SWITCHBOARD_OIDC_ALLOW_LOOPBACK_HTTP` | `false`; explicit local issuer HTTP opt-in. |
| `SWITCHBOARD_URL` | HTTP server URL for remote issue CLI commands. |
| `SWITCHBOARD_OIDC_CLIENT_ID` | Public client ID for browser CLI login. |
| `SWITCHBOARD_CREDENTIALS_FILE` | Override the private CLI credentials cache. |
| `SWITCHBOARD_ACCESS_TOKEN` | Optional issuer-issued access token override. |

Local Pi settings are described below. See `switchboard serve --help` for CLI
options and defaults. Every `/api/v1` request requires an issued access token;
there is no anonymous or shared-static-token API mode. Health/readiness probes
remain unauthenticated. Use HTTPS at the deployment boundary.

## OIDC users and OAuth2 workloads

Switchboard is an OAuth2 resource server; it does not issue tokens. Configure a
provider-neutral OIDC issuer that publishes discovery metadata and JWKS and issues
signed RS256 or ES256 JWT access tokens. The API validates signature, issuer, the appropriate API
resource audience, expiry, and not-before claims. It accepts access tokens, not ID
tokens. Opaque tokens requiring introspection are not supported. API resource
audiences must differ from the public CLI client ID and from one another.

Configure the issuer to grant the user audience only through human login and the
workload audience only through client credentials. Audience separation is a
provider configuration contract; an audience claim alone does not prove the grant
type. Both token kinds use the configured issuer. `switchboard:read` permits API
reads. Human mutations require a user token with `switchboard:write`; workload
tokens are limited to reads in the current API. Execution-specific workload
scopes and callbacks are future work.

Register a public CLI client with authorization code flow, PKCE S256, and a
loopback redirect such as `http://127.0.0.1:8400/callback`. `auth login` opens the
browser and checks callback state and the ID token's nonce before caching issued
credentials. Use `--redirect-port` to change the port; `--no-browser` prints the
authorization URL for manual browser use. `--timeout-seconds` bounds login
(default 300). Its default scopes are
`openid profile offline_access switchboard:read switchboard:write`; override with
`--scopes` if required by the issuer. `--resource` optionally requests the API
resource using RFC 8707. Configure the issuer to issue an access token for the
server's user API audience even when resource selection is implicit.

```bash
cargo run -- auth login --resource https://switchboard.example.com/api/user
cargo run -- issue list
cargo run -- auth logout
```

The CLI automatically refreshes cached user sessions as tokens approach expiry.
Logout removes the local cache; it does not revoke an issuer session or token. `SWITCHBOARD_CREDENTIALS_FILE`
selects a credentials JSON file, defaulting to
`$XDG_STATE_HOME/switchboard/credentials.json` (or
`~/.local/state/switchboard/credentials.json`). Files are private with mode 0600 on
Unix. `SWITCHBOARD_ACCESS_TOKEN` overrides the cache with an already issued access
token for automation; it is not a shared server secret.

For a workload, register an OAuth2 client allowed to use client credentials and
the workload API audience. Supply its client ID and secret at runtime:

```bash
export SWITCHBOARD_OAUTH_CLIENT_ID=automation
export SWITCHBOARD_OIDC_WORKLOAD_AUDIENCE=https://switchboard.example.com/api/workload
# Supply SWITCHBOARD_OAUTH_CLIENT_SECRET through your secret manager.
cargo run -- auth client-credentials \
  --resource https://switchboard.example.com/api/workload
cargo run -- issue list
```

The default workload scope is `switchboard:read`. `--scopes` and `--resource` are
available here too; the issued token uses the same private credential cache.
The client secret is never written to that cache; rerun client credentials with
the runtime secret to obtain a new workload token after expiry. Avoid sharing one
cache between human and workload sessions. No token, secret, or raw claims are logged.

HTTPS is required for normal issuer endpoints. Explicit loopback HTTP opt-in is
limited to literal `127.0.0.1` / `[::1]` fixtures/development and still performs real discovery,
signature, audience, and claim validation; it does not bypass authentication.
The CLI also requires HTTPS for the API URL except for literal loopback addresses.
Tests use a local scripted issuer and never need an external identity provider or
a real model provider.

## Run the complete lifecycle

With serve running and `SWITCHBOARD_URL` configured, create the demo issue:

```bash
cargo run -- issue create --title "Build demo feature" --demo
cargo run -- issue list
```

The persistent fake scheduler creates a child Task and blocks the parent on it.
It completes the child, automatically making the parent Ready, then creates a
human Question and waits. Copy the Question's UUID from `issue list` and answer it:

```bash
cargo run -- issue answer QUESTION_UUID --answer "Use option A"
cargo run -- issue show PARENT_UUID
cargo run -- issue events PARENT_UUID
```

Replace `QUESTION_UUID` and `PARENT_UUID` with the actual IDs printed by the CLI.
The answer completes the Question and unblocks the parent; the running service
then completes the parent automatically. Restarting the service between steps
preserves progress because the Ledger stores child work and answers.

`cargo run -- worker run --until-idle` remains a direct database debugging command.
It drains runnable work and exits while a pending human answer remains; the normal
worker command polls every 500 ms while idle. The legacy direct CLI's default human identity is `operator`, configurable with
the global `--human` option. Remote mutation identity comes from OIDC login.

## Run a local Pi agent

Install Pi separately and configure its provider credentials using Pi's normal
configuration. Use a Pi version whose RPC protocol emits `agent_settled`; older
versions that only emit `agent_end` are not supported. The implementation follows
[Pi's current RPC documentation](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md).
No model-provider SDK or vendor credential configuration is added to Switchboard.

Stop the fake server, then start the local Pi server:

```bash
cargo run -- serve --backend pi-local --workspace /absolute/path/to/repository
```

From the remote CLI terminal, create work for that server:

```bash
cargo run -- issue create --title "Fix failing tests" --description "Run the tests and fix the failures"
```

Infrastructure options are separate from durable Agent identity. Serve and the
debugging worker accept `--backend pi-local` (`pi` is a compatibility alias) and
`--agent`, or the corresponding environment settings:

| Option | Environment | Default |
| --- | --- | --- |
| `--backend fake\|pi-local` (serve) | `SWITCHBOARD_BACKEND` | `fake` |
| `--workspace` | `SWITCHBOARD_WORKSPACE` | Required for Pi |
| `--pi-binary` | `SWITCHBOARD_PI_BINARY` | `pi` |
| `--pi-provider` | `SWITCHBOARD_PI_PROVIDER` | Pi's configuration |
| `--pi-model` | `SWITCHBOARD_PI_MODEL` | Pi's configuration |
| `--instructions` | `SWITCHBOARD_AGENT_INSTRUCTIONS` | Complete the assigned task and report its result. |
| `--execution-timeout-seconds` | — | `3600` |

The directory must exist. Each attempt starts a fresh
`pi --mode rpc --no-session` subprocess in that directory; provider/model options
are passed as arguments. Invalid configuration and missing executables fail clearly;
Pi never falls back to the fake backend. A startup or protocol failure records a
fenced Failed outcome, while configuration errors fail before a worker starts.

Switchboard owns durable coordination; agent backends own ephemeral agent execution.
The persistent scheduler is orchestration; AgentBackend is execution.
The backend receives agent instructions, Issue identity/title/description, and
parent, child, and prerequisite snapshots, without database handles. Pi manages
models, tools, context, retries, and compaction. It receives a bounded prompt
(32 KiB; at most 8 children and 8 prerequisites), and returns a final summary
(up to 8 KiB, on UTF-8 boundaries). The completion summary is stored in
`IssueCompleted` event metadata in the same fenced transaction as completion.
A final summary starting with `SWITCHBOARD_FAILED:` reports explicit task failure.
Pi execution errors also produce failure outcomes; aborted work has no completion.

Prompt acceptance and `agent_end` are not completion: Switchboard drains events
until `agent_settled`, including retries and compaction, then closes stdin and waits
for process exit before applying the result. Pi session files are not durable
Switchboard state. This milestone exposes no Ledger mutation, delegation, or
human-question tools to Pi; those remain available in the deterministic demo only.

Lease loss, a backend deadline, or Ctrl-C requests abort and closes input. The
supervisor grants bounded shutdown time, then kills and reaps an uncooperative
process. Dropping an execution future signals the same cleanup. On Unix, the
process has its own group and remaining group members are killed during cleanup.
On other platforms, cleanup covers the direct Pi child. Local execution is not an
isolated sandbox and cannot undo file/shell effects already performed. Cancelled
attempts stop renewing and can subsequently be recovered after lease expiry.

Tracing records attempt IDs, process IDs, startup, acceptance, settlement, abort,
exit, and backend failure. `RUST_LOG=runtime=info` selects runtime tracing. Stderr
is drained as diagnostics without parsing or recording it; prompts, credentials,
raw conversations, and thinking content are not logged.

## Inspect and manage work

```bash
cargo run -- issue create --title "A simple task"
cargo run -- issue list --ready
cargo run -- issue show ISSUE_UUID
cargo run -- issue events ISSUE_UUID
cargo run -- issue attempts ISSUE_UUID
cargo run -- worker recover
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
Ready work; worker-owned execution uses attempt-fenced Ledger operations. `issue show` includes
`current_attempt_id`; `issue attempts` prints each attempt's peer, state, timestamps,
and lease expiry. Historical attempts remain available after recovery or completion.

## Leases and worker recovery

Claims lease work for 30 seconds by default. Configure the duration for claims and
renewals with global `--lease-seconds` or `SWITCHBOARD_LEASE_SECONDS` (CLI: 1–86400
seconds; Ledger API: 3 milliseconds–1 day):

```bash
cargo run -- --lease-seconds 60 worker run --agent another-worker
cargo run -- worker recover
cargo run -- issue attempts ISSUE_UUID
```

Workers renew every third of the lease duration while loading context and awaiting
execution. Every tick recovers expired Claimed/Running work before claiming; a
polling worker therefore repairs disappeared workers automatically. `worker recover`
also runs recovery explicitly without executing work. An idle worker that already
exited with `--until-idle` cannot perform later recovery; start another worker or
run maintenance. Recovery preserves the expired attempt and returns the Issue to
Ready (or waiting if prerequisites require it). Reclaiming creates a new attempt,
even when the same Peer returns. Late completions, failures, and child/human
handoffs from previous attempts are rejected.

A heartbeat failure cancels the backend, allows bounded cleanup, and ends the tick with an error;
no detached heartbeat task survives. Completing or handing off work closes the
attempt transactionally. Repeating an outcome conflicts before adding events or
children. Heartbeats update attempt rows without flooding Issue events.

## HTTP API and deployment

The versioned JSON API uses UUID identities and controlled Ledger operations:

| Method | Route | Purpose |
| --- | --- | --- |
| POST / GET | `/api/v1/issues` | Create / list Issues. |
| GET | `/api/v1/issues/:id` | Get an Issue. |
| GET | `/api/v1/issues/:id/events` | Inspect its durable event history. |
| GET | `/api/v1/issues/:id/attempts` | Inspect execution attempts. |
| POST | `/api/v1/issues/:id/ready` | Make backlog work runnable. |
| POST | `/api/v1/issues/:id/complete` | Apply a valid human completion. |
| POST | `/api/v1/issues/:id/cancel` | Cancel with a reason. |
| POST | `/api/v1/issues/:id/answer` | Resolve a Question or Approval. |
| POST | `/api/v1/issues/:id/dependencies` | Add a prerequisite. |
| GET | `/api/v1/status` | Safe version/scheduler/backend information. |
| GET | `/api/v1/me` | Verified caller identity and durable Peer ID. |

Mutation actors come from the verified issuer/subject identity, mapped to a
durable Peer; caller-supplied `actor` fields are rejected. Requests use
`Content-Type: application/json`. Create accepts `title`,
`description` (default empty), `kind` (`Task`, `Question`, or `Approval`, default
`Task`), `priority` (default 0), and `backlog` (default false).
Ready and complete accept `{}`; cancel requires
`reason`, answer requires `answer`, and dependencies requires `dependency_id`.
Unknown fields, including a proposed arbitrary `status`, are rejected. Mutation
responses are Issue objects, except dependency addition returns 204 with no body.
List returns an array and accepts `?ready=true`; events and attempts return arrays.
Request bodies are limited to 64 KiB. List and history pagination remain future work.

```bash
curl -f -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $SWITCHBOARD_ACCESS_TOKEN" \
  -d '{"title":"HTTP intake","backlog":true}' \
  "$SWITCHBOARD_URL/api/v1/issues"
```

This curl example assumes `SWITCHBOARD_ACCESS_TOKEN` holds an issuer-issued user
access token with `switchboard:write`. Unknown Issues return 404; invalid
transitions and domain conflicts return
structured client errors rather than 500. Errors have the shape
`{"error":{"code":"issue_not_found","message":"..."}}` and exclude SQL,
connection information, and internal diagnostics. Every `/api/v1` route requires
`Authorization: Bearer <access-token>`. Missing or invalid tokens return 401;
insufficient scopes or a workload mutation return 403.

`GET /healthz` reports process liveness. `GET /readyz` checks the database,
required schema, and bounded issuer/JWKS availability. Fresh cached signing keys
remain usable during an issuer outage; readiness fails when necessary dependencies
are unavailable. Neither endpoint migrates or mutates the Ledger. Request tracing records method, route, status, latency, and
request ID without request bodies. Attempt IDs remain in execution tracing.
A global SSE stream is deferred until durable, resumable event cursors and bounded
polling can be implemented coherently; per-Issue history is available now.

Deploy persistent PostgreSQL plus one Switchboard server replica initially. Run
`switchboard migrate` as an explicit deployment step, configure OIDC, then run
`switchboard serve`. Register separate user and workload resource audiences and
restrict their grants at the issuer before exposing the API.
The existing non-root Docker image runs `serve` by passing it as the container
command; no additional packages are needed for HTTP or fake execution. Provide
secrets at runtime, expose port 8080, and configure liveness/readiness probes on
`/healthz` and `/readyz`. Allow a termination grace period that covers bounded
backend cleanup. Pi local needs its executable, workspace, and normal provider
configuration supplied separately; the base image does not install Pi.

Ctrl-C and SIGTERM stop HTTP acceptance and new claims, cancel active executions,
stop heartbeats, and bound backend cleanup. Interrupted work is never
marked successfully completed. Its lease expires in PostgreSQL; the restarted
scheduler records recovery and reclaims work with a new fenced AttemptId. Active
slots each have their own attempt, heartbeat, and cancellation lifecycle. Atomic
PostgreSQL claims prevent double ownership; full multi-replica HA lifecycle
behavior is not claimed. Later execution control may target Kubernetes/gVisor
agent pods without changing the durable control plane.

## Workspace

```text
crates/ledger/         Domain types, controlled operations, PostgreSQL persistence
crates/runtime/        AgentBackend, Worker, Pi RPC, local transport, fake executor
crates/control-plane/  Shared operations, HTTP API, persistent scheduler
src/                   CLI, remote client, and server wiring
migrations/            PostgreSQL schema
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
attempt fencing, lease renewal and recovery races, long-running worker heartbeats,
and event history.

The `rpc-fixture` feature builds a scripted Rust child solely for deterministic
RPC tests; all-feature checks enable it. These tests cover JSONL framing, interleaved
events, settlement, retries, bounded results, protocol errors, unexpected exits,
diagnostic isolation, abort, deadlines, forced termination, and future-drop cleanup.
Pi/Worker integration tests also verify context, summary persistence, lease loss,
and model changes using the same Peer. No test calls a real model provider.

The repository retains pinned tooling, Nextest archives, formatting and Clippy
policy, dependency auditing, Renovate, release automation, signed release assets,
and a nonroot container build. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Current limits

- Pi runs locally with its normal coding tools and provider configuration. Pi has
  no Switchboard coordination tools yet. Kubernetes, gVisor, workspace cloning,
  and native model-provider integrations are future work.
- Lease recovery retries interrupted work, but explicit Failed work still needs
  human intervention. There is no retry budget or backoff for repeated crashes.
- Dependency and lifecycle mutations use a coarse transaction advisory lock.
  Concurrent claims remain safe; write throughput is intentionally modest.
- Only Completed satisfies dependencies. Failed or Cancelled prerequisites leave
  dependent work blocked for explicit intervention.
- Approval resolution records decision text without enforcing approve/reject
  policy. OIDC subjects map to durable Peers, but fine-grained ownership/RBAC is
  not implemented; authorized human writers share the human-work inbox.
- No web frontend, global event stream, chat integration, multi-agent routing,
  distributed leader election, Swarm, memory system, general retry policy, or
  elaborate permissions are implemented.

See [the architecture note](docs/architecture.md) for state semantics, transaction
boundaries, and open design questions. External effects still need durable idempotency and
checkpointing: use `(attempt_id, operation identity)` for each effect within one
attempt. This milestone fences Ledger writes; it cannot undo an external effect
already performed by a worker that later loses its lease.
