# Architecture

## Work lives in the Ledger

An Issue is the unit of actionable work: a Task, Question, or Approval. The
Ledger stores current state, dependencies, parentage, and an append-only event
history in PostgreSQL. Conversation history is never authoritative task storage.
The current-state tables are authoritative; events explain changes without
requiring event replay or introducing full event sourcing.

A Peer is a durable human or agent identity. It has no model or provider. The
runtime's Agent and Executor describe who performs work and how execution runs;
future provider adapters belong behind the executor boundary, outside the domain.

## Components

- `crates/ledger`: typed UUID identities, domain types, controlled lifecycle
  operations, migrations, and SQLx persistence.
- `crates/runtime`: agent definition, worker loop, provider-independent execution
  outcomes, and deterministic fake executor.
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

Domain operations validate transitions and claimant ownership. They update state
and append structured events in the same transaction. Callers cannot assign an
arbitrary status through the Ledger API. Workers do not execute Questions or
Approvals.

## Claiming and transaction boundaries

Ready Tasks are ordered by descending priority, creation time, then ID.
`claim_next_issue` uses PostgreSQL row locks with `FOR UPDATE SKIP LOCKED` and
updates the selected row and event history in one transaction. Concurrent
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

## Execution and recovery

The worker claims, starts, executes, then applies a structured outcome. The fake
executor exercises child work and human input using persisted Ledger state, so
restarting a worker does not lose the demo's progress. No LLM, provider account,
conversation session, or external tool is needed.

There are no claim leases, retries, or automatic recovery of an interrupted
Running issue. A crash can leave work Claimed or Running; cancel it explicitly
and create replacement work after inspecting history. The process is intended
for trusted local peers; peer IDs are not authentication credentials.

## Open questions and future boundaries

1. Claim leases, fencing, heartbeats, and recovery of interrupted execution.
2. Idempotent executor effects and durable checkpointing beyond Ledger mutations.
3. Richer human decisions, rejection, reassignment, and failed dependency handling.
4. Agent capability selection and fairness without coupling identity to providers.
5. Provider adapters and execution tools behind the Executor boundary.
6. Finer-grained graph concurrency if measured contention warrants it.

The migrations replace the old Switchboard schema. They are a clean start and do
not implement an in-place migration from the former application. Use a new
database; any deliberate deletion of an old database is a separate operator action.
