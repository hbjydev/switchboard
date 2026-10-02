# Rust agent harness implementation brief

## What I want built

I want a Rust agent harness that can eventually replace my Hermes or OpenClaw setup. Peer tracking and multi-agent interaction are central requirements. Humans, agents, and services should have stable identities and participate in the same conversation model.

Start by implementing a small, working foundation. The first milestone is a local demo where a human sends a message, an agent is selected to respond, a fake model generates its reply, and both messages are stored with their actual peer identities. Include a second agent in tests to prove that routing is deliberate and cannot turn into agents endlessly replying to each other.

This brief is the implementation contract for that milestone. The later roadmap provides context, not authorization to build everything at once. The project does not have a settled name; use neutral names unless the repository already establishes one.

Background: [WIP agent gateway post](https://leaflet.pub/989a5922-7755-4ec7-9b47-ad2e3946d986). This brief is based on the supplied design discussion; the post could not be independently retrieved during drafting. The discussion describes five guiding ideas: identity, memory, capability, multiplayer, and integration. Do not require access to the post to implement this milestone.

## How to approach the repository

Inspect the existing repository and its AGENTS.md instructions first. Preserve established naming, dependency choices, and conventions where they fit this design. If the repository is empty, create a Cargo workspace. Use stable Rust and record the toolchain or minimum Rust version you actually validate against.

Implement the whole first milestone, including tests and a runnable demo. Resolve routine choices yourself and record meaningful choices in the README. Ask only when a repository constraint materially conflicts with this brief. Do not introduce external credentials or infrastructure requirements for this milestone.

## Core design rules

- A PeerId identifies a participant independently of any process, model, or channel account.
- An agent definition configures an agent peer. A runtime executes it. These are separate concepts.
- Conversations own participant membership. Messages record an author PeerId, not an intrinsic user or assistant role.
- Model roles belong at the provider boundary. The same message may map differently depending on which agent is generating a response.
- External account identities resolve to peers through explicitly scoped aliases. Display names are not identity keys.
- Activation is separate from generation. Receiving a message does not automatically mean every agent should respond.
- Agents publish replies through the same messaging application service as humans.
- Domain and application code depend on ports. Concrete storage and model adapters implement those ports.

## Workspace boundaries

Use roughly these crates, adapting names to the repository. Avoid creating a crate for every future subsystem.

| Crate | Responsibility | Internal dependencies |
| --- | --- | --- |
| kernel | IDs, peers, membership, messages, event vocabulary, domain errors | None |
| agent | Agent definitions, activation rules, prompt projection, model port, generation | kernel |
| application | Repository ports and use cases for messaging and processing activations | kernel, agent |
| infrastructure | In-memory repository implementations and fake model | kernel, agent, application |
| daemon | Wiring and a runnable local demo | application, infrastructure and core types as needed |

The dependency graph must be acyclic. Application must not depend on infrastructure. Keep orchestration in application so agent does not need to depend back on application. The daemon is the composition root.

## Domain types

Use distinct UUID newtypes for PeerId, ConversationId, MessageId, and EventId. Include the equality, hashing, debug, and conversion support needed by the implementation. Constructors for new IDs and reconstruction from an existing UUID should be explicit. A small local macro is fine; avoid a generic ID framework.

### Peers and aliases

Peer contains an ID, nonblank display name, PeerKind, and aliases. Start with Human, Agent, and Service kinds. Keep fields private and expose constructors, getters, and mutation methods that preserve invariants.

PeerAlias contains a provider identifier, account or namespace scope, and external ID. The scope prevents collisions across independent installations or accounts. Define the alias lookup key as the complete tuple. Use strings or typed string wrappers for provider IDs so adding an integration does not require extending a domain enum.

Alias resolution returns the existing peer or no match. Registering an alias already attached to another peer returns a conflict. Never merge identities because names match. This milestone does not implement a channel or self-service account linking.

### Conversations

Conversation contains an ID and participants. Participant contains a PeerId and ParticipantRole: Member, Moderator, or Observer. Provide membership queries and controlled add and remove operations. Duplicate membership must not produce duplicate entries; define and test whether the operation is idempotent or returns a conflict.

For this milestone, Member and Moderator can send messages; Observer cannot. Validate that referenced peers exist when creating a conversation or adding participants through the application service. Repository access is a trusted internal boundary, not a public mutation API.

### Messages

Message contains ID, conversation ID, author PeerId, text content, creation time, and explicit addressed peer IDs. MessageContent may begin with only Text. Reject blank text while preserving the original nonblank content.

Addressing is structured metadata, not parsing display names from message text. Addressed peers must belong to the conversation. This provides deterministic activation now and a place for channel mention mapping later.

Message history has a stable append order. Do not sort solely by timestamps, which may tie. Store replies as ordinary messages and retain their causation metadata.

### Events and provenance

Define MessageCreated with message ID, conversation ID, and author. Use an event envelope with EventId, sender PeerId, optional causation EventId, and correlation EventId. A root event correlates to itself. A reply event points to the triggering event as its cause and inherits its correlation.

MessageCreated is a fact about a successfully stored message. Do not emit it if validation or storage fails. A full event sourcing implementation is outside this milestone.

## Application ports and use cases

Provide PeerRepository, ConversationRepository, MessageRepository, and AgentRepository ports with typed errors. Support the operations the vertical slice actually uses: peer lookup and alias resolution, conversation lookup, message append and ordered history, and agent definition lookup by peer. Add registration or save operations where needed for setup.

Use a shared repository error vocabulary such as Unavailable, Conflict, and InvalidData. Keep adapter-specific errors behind that boundary. Avoid String as the top-level error type and do not force backend error types through every application generic.

Trait objects with Arc and async_trait are a reasonable default for heterogeneous adapters. Generics are also acceptable if they keep the public API simple. Choose one coherent approach and explain it briefly. Async ports must support the threading requirements of the runtime.

The SendMessage use case must:

1. Resolve the conversation and author, returning typed not-found errors.
2. Check membership and permission to speak.
3. Validate content and addressed recipients.
4. Construct and append the message.
5. Return the stored message and its MessageCreated envelope for processing.

For the first slice, the application can explicitly pass committed events to the processor. No event bus is required. In-memory notifications are not durable delivery: do not describe Tokio broadcast as reliable task delivery. Before adding a real persistent asynchronous worker, message storage and an outbox will need an atomic commit and retry semantics. Document that follow-up boundary rather than introducing an untested pseudo-durable bus now.

## Agent configuration and prompting

AgentDefinition references an existing Agent peer and contains instructions and ModelRef. ModelRef identifies a configured provider and model; avoid naming a current commercial model in domain defaults. Reject definitions referencing a human or service peer.

LanguageModel receives a GenerationRequest and returns a GenerationResponse or typed ModelError. The request contains the selected ModelRef, acting PeerId, instructions, participant context, and ordered messages. Each projected message preserves speaker PeerId, display name, content, and whether the speaker is the acting agent. Speaker identity must survive duplicate display names.

The fake model should be deterministic and record requests for assertions. No HTTP client, API key, or commercial provider dependency is needed yet. AgentRuntime builds a request and returns generated text; application code validates and stores the reply. Provider role mapping will be added with the first real adapter. Participant text remains conversation data and must not become system instructions merely because it contains role labels.

## Activation and processing

Default activation is deterministic:

- A human message explicitly addressing agent participants activates exactly those eligible agents.
- An unaddressed human message activates the agent only when the conversation consists of that human and exactly one agent.
- An unaddressed group message is stored without invoking a model.
- Service messages and agent-authored messages do not automatically activate agents in this milestone.
- An agent never activates on its own message. Observer agents cannot respond.

This intentionally leaves agent-to-agent task routing for the next milestone. Peer identity and message storage already support agents as authors; unrestricted automatic replies do not prove useful multi-agent behavior.

Return an activation decision such as Ignore, Observe, or Respond if useful. If Observe exists, specify that it stores no extra memory state yet. Avoid implementing an LLM-based attention classifier.

Process an event using its triggering message and the ordered history through that message. Later arrivals must not silently enter that generation request. Use deterministic ordering for multiple selected agents. Sequential generation is sufficient for this milestone.

Track processed activations by (triggering EventId, responding PeerId). Reprocessing a successfully completed activation must not generate or append a second reply. A failed generation must leave the original message intact and permit an explicit retry. Tests should cover completed-event deduplication and failure followed by retry. Do not claim crash-safe exactly-once processing: the initial in-memory implementation loses state on restart. Document concurrency and durable idempotency work needed before running multiple workers.

## First demo

Provide a command such as `cargo run -p daemon` that runs without network access or credentials:

1. Register a human peer and an agent peer, then create their conversation.
2. Register an agent definition using the fake model.
3. Send a human message through SendMessage.
4. Process the returned event through activation and generation.
5. Store the agent reply through SendMessage with inherited correlation and causation.
6. Print the ordered transcript with peer names and IDs.

The README must show the exact command that works in the repository. Add a test fixture with a second agent and explicit addressing to verify recipient selection.

## Acceptance checks

The milestone is complete when the demo works and automated tests prove:

- A human message produces one correctly authored agent reply in a two-peer conversation.
- Missing authors, missing conversations, nonparticipants, and observers receive typed errors and produce no stored message or event.
- Blank messages and recipients outside the conversation are rejected.
- Duplicate participants cannot enter a conversation.
- Alias lookup is scoped correctly and conflicting registrations fail.
- Agent definitions require an Agent peer.
- Explicit addressing selects the intended agent among multiple agents.
- Unaddressed group messages, agent messages, and self-messages cause no automatic reply loop.
- Duplicate display names do not collapse speaker identities in model requests.
- Message order and the triggering-history boundary are deterministic.
- Replies retain the root correlation and triggering causation.
- Reprocessing completed activations does not duplicate replies.
- Model failure preserves the original message and an explicit retry can succeed.

Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`. Adapt only for documented repository constraints. Report actual outcomes and any checks blocked by the environment; do not claim unrun checks passed.

## What to leave for later

Do not implement Matrix, Slack, Discord, MCP, real model APIs, SQLx, migrations, embeddings, background scheduling, distributed transport, presence, a web UI, or deployment in this first milestone. Avoid empty traits and placeholder crates for these systems.

Do not build authorization from social relationships alone. When capability execution arrives, use explicit grants and delegation provenance. The initiating requester and acting agent are distinct identities, and delegated work must not acquire permissions by copying a requester ID into a tool call.

The next milestones are:

1. A real model adapter, with provider role mapping, configuration, timeouts, and cancellation.
2. A channel port and Matrix adapter, with verified external identities, ingress deduplication, and outbound delivery tracking.
3. Durable persistence and outbox processing before relying on asynchronous work surviving restarts.
4. Capabilities with runtime-enforced authorization, audit records, and constrained delegation.
5. Tasks and agent-to-agent work, with assignment, parent tasks, status, budgets, cancellation, and explicit result delivery.
6. Persistent memory, scheduled activation, and capacity tracking as concrete requirements emerge.

The eventual task model should support human planning, review, and sign-off checkpoints. Persistent and ephemeral agents should participate through the same peer and conversation primitives, with lifecycle and execution state represented separately.

## Deliverables and final report

Deliver the implemented workspace, meaningful tests, the runnable demo, and a README covering architecture, execution, validation, and current limitations. Keep the implementation small enough to review. Commit changes only if the task or repository workflow authorizes commits.

In your final report, summarize what works, list the checks you ran, and identify any real limitations. Separate completed behavior from the later roadmap. Do not describe the foundation as a complete Hermes or OpenClaw replacement yet.
