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
- `src/main.rs`: Clap CLI, configuration, and wiring.
- `migrations`: clean PostgreSQL schema for this application.

There is deliberately no Swarm object. Multiple workers coordinate by claiming
ordinary Ledger work. A scheduling hierarchy is unnecessary for this milestone.

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
not a distributed scheduling framework. All mutations must go through the Ledger
API to preserve these invariants.

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
They form an unassigned shared inbox in this milestone; any trusted human can
resolve them. A worker can create one and depend on it. The CLI records a nonempty human answer, completes
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
the Issue, including before recovery. IDs assume trusted callers and are not
authentication credentials. Human administration uses human-only operations without
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
first renewal is delayed, missed ticks use Delay, and renewal wins simultaneous
readiness. No detached heartbeat task exists: dropping the tick or a failed renewal
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
maintenance without execution; normal polling workers also perform recovery.

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
