# Switchboard

[![CI](https://github.com/hbjydev/switchboard/actions/workflows/ci.yaml/badge.svg)](https://github.com/hbjydev/switchboard/actions/workflows/ci.yaml)

A persistent AI harness. The first local slice sends a human message, selects an
agent, generates a fake-model reply, and stores both with their peer identities.

Run the demo through the workspace's sole executable, `switchboard` in
`crates/main`:

```sh
mise exec -- cargo run -p switchboard -- demo
mise exec -- cargo run -p switchboard -- demo --message "Hello from the harness!"
```

The demo uses no network services or credentials. After dependencies are cached,
add Cargo's `--offline --locked` flags before `--` to build and run offline.
It prints the ordered transcript with names and UUIDs. Each invocation seeds a
fresh human, agent, definition, and conversation; no data survives exit. Blank
messages fail with a nonzero exit code and no transcript. Global `--log-format`
remains available; diagnostics use stderr. There is no server command yet.

| Crate | Responsibility | Internal dependencies |
| --- | --- | --- |
| `uuids` | Distinct participant, conversation, message, and event IDs | None |
| `kernel` | Peers, membership, messages, events, invariants | uuids |
| `agent` | Definitions, activation, projection, model port, runtime | kernel, uuids |
| `application` | Repository ports, message publication, event processing | agent, kernel, uuids |
| `infrastructure` | In-memory repositories and fake model | application, agent, kernel, uuids |
| `main` (`switchboard`) | CLI and composition root for executable flows | application, infrastructure, agent, kernel, uuids |

The dependency graph is acyclic. Library crates contain reusable behavior;
executable entry points and adapter wiring belong in `crates/main`.

The current foundation lives in `switchboard-kernel`, with distinct UUID types
in `switchboard-uuids`. Peer creation and renaming reject blank names; message
construction rejects blank text and preserves valid content unchanged. Aliases
use the complete `(provider, scope, external_id)` tuple as their identity.
Conversation membership preserves insertion order and is idempotent: adding an
existing participant keeps their role. Members and moderators can send;
observers cannot.

Event envelopes provide root and reply constructors. Roots correlate to their
own ID; replies inherit correlation and identify the immediate triggering event
as their cause. The application layer must create message-created events only
after a successful append, retain the message and its envelope together, and
validate peer existence, membership, addressed recipients, and cross-peer alias
uniqueness. Ordered history will use repository append order rather than time.
The local conversation demo is wired through `switchboard demo`.

Rust 1.98.1 is pinned in `.mise/config.toml` and `rust-toolchain.toml`. Validate with:

```sh
mise run fmt-check
mise run clippy
mise run test
mise run complexity-check
mise run build
```

`switchboard-application::repository` defines async repository ports for peers,
conversations, messages, and agent definitions. They use `async_trait` with
`Send + Sync` bounds so services can share heterogeneous adapters through
`Arc<dyn Trait>`. Lookups return `Option` for absence; storage failures use
`RepositoryError::{Unavailable, Conflict, InvalidData}` without exposing backend
error types.

Peer saves must enforce scoped alias ownership atomically. Message appends
store a `StoredMessage` (message plus envelope) together, reject duplicate IDs
and mismatched event data, and preserve append order. `history_through` returns
an inclusive prefix ending at the triggering message, excluding later arrivals.
In-memory implementations and their behavioral tests live in
`switchboard-infrastructure`. Durable delivery still requires an outbox and retry
semantics before introducing persistent asynchronous workers.

`switchboard-agent` separates configuration (`AgentDefinition` and `ModelRef`),
activation selection, prompt projection, and runtime generation. Definition
construction requires an Agent-kind peer; application setup must resolve that
peer from storage.

Enable the application crate's optional `mocks` feature to expose
`MockPeerRepository`, `MockConversationRepository`, `MockMessageRepository`,
and `MockAgentRepository` in `switchboard_application::repository`. Mocks are
generated with Mockall and are disabled by default. Consumers can enable them
in a dev-dependency:

```toml
[dev-dependencies]
switchboard-application = { path = "../application", features = ["mocks"] }
```

Configure `expect_*` methods with argument matchers, call counts, and return
values before passing a mock as `Arc<dyn PeerRepository>` (or another port).
For async methods, expectation closures return the method's result directly.
Run the mock tests with
`mise exec -- cargo test -p switchboard-application --features mocks`.
Mocks verify caller behavior; in-memory adapters still need separate tests for
storage ordering, atomic writes, and alias uniqueness.


`switchboard_infrastructure::memory` provides `InMemoryPeerRepository`,
`InMemoryConversationRepository`, `InMemoryMessageRepository`, and
`InMemoryAgentRepository`. Construct each with `default()` and share that instance
via `Arc` to retain state across callers. Independent instances have independent
state; restarting loses all data. Read operations return owned snapshots.

Each repository serializes operations with a short mutex lock, with no await
while locked. Peer saves check all alias ownership before replacing the peer
and its alias index. Message appends validate the message-created payload,
sender, and basic envelope provenance before committing the message, event ID,
and history position together. History reads clone a consistent append-order
snapshot, including only the inclusive prefix requested by `history_through`.
Poisoned locks map to `RepositoryError::Unavailable`.

These adapters are trusted storage boundaries: application services still
validate peer existence, membership, and speaking permissions. Operations across
different repositories are not transactions, and retained events are not a
persistent outbox. Reply provenance retains the supplied cause and correlation;
validating the triggering event is the application processor's responsibility.

`switchboard_application::messaging::SendMessage` is the common publication
service for every peer kind. Construct it with shared peer, conversation, and
message repositories, then call `execute(SendMessageRequest)`. It checks the
conversation and author exist, membership and role permit speaking, text is
nonblank, and addressed peers are participants. Typed `SendMessageError` values
distinguish rejected input, missing records, and repository failures.

A successful send returns `StoredMessage` only after append succeeds. Optional
`reply_to` names a stored message in the same conversation; its retained event
supplies immediate causation and inherited correlation. Callers cannot supply
an arbitrary envelope as reply provenance. No event bus is involved. Membership
validation and append span separate repositories, so concurrent membership
changes are not transactionally isolated in this local milestone.

`switchboard_agent::activation::responding_agents` is a pure selection policy.
Application code supplies a conversation, its triggering message, the resolved
author, and registered agent definitions. Human messages explicitly addressing
agents select exactly those configured participants allowed to speak, in
conversation membership order. Unaddressed human messages select an agent only
in a two-participant conversation containing that human and one configured
agent. All unaddressed group messages, service messages, and agent messages
select nobody. Observers cannot respond; moderators can. Names and text never
act as addressing, and no observation or memory state is written. The application
processor connects these decisions to generation.

`switchboard_agent::model::LanguageModel` is an async, `Send + Sync` generation
port. `AgentRuntime` projects the supplied history into a `GenerationRequest`
and returns generated text without storing messages. Requests carry the selected
`ModelRef`, acting peer, configured instructions, participant context, and
ordered messages with speaker IDs, names, content, and an acting-agent flag.
Duplicate names never collapse identities, and another agent's messages remain
attributed to that agent. Participant text, including role-like labels, remains
conversation data; provider role mapping lives in the OpenAI adapter described below.

The caller supplies the inclusive history through the triggering message and
resolves both current participants and historical speakers. Projection preserves
that order, supports speakers who have left, and rejects missing peer context,
messages from another conversation, and acting agents unable to speak.

`switchboard_infrastructure::fake_model::FakeModel` returns deterministic replies
without network access or credentials. `requests()` returns owned snapshots of
all generation attempts, including failures; `enqueue()` supplies the next
response or typed model failure for tests. Generated text is validated when the
application publishes it through `SendMessage`. The application processor connects generation to publication and completed
activation tracking; the runnable demo is wired through `switchboard demo`.


`switchboard_application::processing::ActivationProcessor::process` takes a
committed envelope, verifies it against stored provenance, and processes selected
agents sequentially in membership order. It loads history through the triggering
message once, resolves current participants and historical speakers, generates,
and publishes each reply through `SendMessage`. Later arrivals and earlier
agents' replies do not enter another agent's request for the same trigger.

Keep one processor instance and call it mutably to serialize processing. Successful
activations are tracked by `(EventId, responding PeerId)` only after publication.
Reprocessing skips those completions. Model failure, invalid output, or failed
publication preserves the original message and permits an explicit retry. If a
later agent fails, earlier replies stay committed; the call returns an error,
and retry returns only newly committed replies. Committed partial results remain
available in history. Ignored events return an empty reply list.

Completion tracking is in-memory and private to that processor. Recreating it
loses deduplication state. Multiple processor instances sharing repositories,
cancellation during publication, and crashes are not covered by exactly-once
semantics. Durable idempotency, transactional outbox delivery, and worker claims
are required before operating persistent concurrent workers. Conversation
membership and agent configuration are read at processing time rather than
historically versioned at the trigger.

The OpenAI adapter uses the [Responses API](https://developers.openai.com/api/docs/guides/text)
through a small Rustls-backed HTTP client. Run a one-shot real-model conversation:

```sh
# Set OPENAI_API_KEY in your shell or the Mise-loaded .env file first.
mise exec -- cargo run -p switchboard -- openai --model YOUR_MODEL_ID --message "Hello!"
```

There is no default commercial model. `OPENAI_MODEL` can replace `--model`.
`--instructions` supplies the agent definition's instructions. The API key is
read only from `OPENAI_API_KEY`, never a CLI argument. `--timeout-seconds`
(or `OPENAI_TIMEOUT_SECONDS`) defaults to 60 and must be positive. Ctrl-C cancels
the adapter's pending generation, returning a nonzero exit status. The fake
`demo` command remains credential-free. Both commands still use fresh in-memory
repositories; the OpenAI command is not a persistent server or interactive chat.

`OpenAiConfig::new` validates the API key and timeout; `OpenAiModel::new` constructs
the adapter. `--base-url` / `OPENAI_BASE_URL` optionally overrides the API prefix
(default `https://api.openai.com/v1`); `/responses` is appended. URLs must use HTTPS
or loopback HTTP and contain no credentials, query, or fragment. The configured
endpoint receives the API key. Redirects are disabled, and errors contain neither
credentials nor provider response bodies.

Provider mapping is relative to the acting peer: only its own previous messages
use the assistant role; humans, services, and other agents use the user role.
An initial user input contains participant context and the acting peer ID. Each
history message carries JSON-encoded speaker ID, message ID, display name, and
original text, preserving duplicate names and departed speakers. Only configured
instructions populate the API's `instructions` field. Participant names, roles,
and role-like text stay input data. This encodes identity and instruction priority;
it is not a guarantee that a model will resist every prompt injection.

Requests include the entire supplied history, use `store: false`, and do not use
provider conversation IDs or `previous_response_id`. The adapter reads all
assistant `output_text` parts in order, ignoring reasoning items. It rejects
incomplete, malformed, empty, refused, or unsupported output instead of publishing
a partial reply. Streaming, tool calls, images, and reasoning-state replay are
outside this adapter's current text-only scope.

`ModelError` distinguishes invalid requests, authentication/authorization,
rate limits, provider unavailability, timeout, cancellation, invalid responses,
and refusal. There are no automatic HTTP retries. The application processor
retains the original human message on failure and permits an explicit retry;
successful activations retain existing process-local deduplication semantics.
The CLI exits on failure, so its ephemeral state cannot be retried in another
invocation. HTTP timeout covers generation through response-body decoding.

`OpenAiModel::cancellation_token()` returns an adapter-lifecycle shutdown token:
cancelling it stops all current and future generations on that instance. Create
a new adapter for a new lifecycle. Dropping a generation future also stops local
waiting. Cancellation or timeout does not guarantee that the provider stops
remote computation, and interrupted publication retains the foundation's existing
idempotency limitations.

Adapter tests use an ephemeral loopback mock server with dummy credentials. They
check the HTTP schema, identity/role mapping, typed errors, response parsing,
timeouts, cancellation, and application retry/deduplication. No test calls OpenAI
or reads the user's API key:

```sh
mise exec -- cargo test -p switchboard-infrastructure --test openai --locked
```
