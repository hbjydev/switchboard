# Agentic Coding Guidance for Switchboard

Guidance for agents (and humans) working in the Switchboard repository.

## What this is

Switchboard is a persistent agent framework written in Rust.

## The one load-bearing(tm) idea: persistence

A persistent agent must implement a few key concepts, which is the goal of this
project:

1. **Identity-aware:** The agent should know what it is, what it's here for,
   and what its boundaries are.

2. **Memory-full:** As the agent is used, and as such grows, it should remember
   things, much like a junior engineer learns a company's standards as they
   interact more with the tools, culture, and documentation.

3. **Capable:** The agent should be able to perform the tasks you give it
   autonomously, and should therefore have access to all of the tools and data
   it needs to do so, like your observability stacks, your data platform, your
   Git forges, etc.

4. **Social:** A persistent agent's key supporting pillar would be its
   understanding that it is one of many participants in any given conversation;
   our current agents know they are being prompted by "the user", or "the
   admin", etc., but know nothing about the fact that "Hayden is talking to me",
   "I'm in a group chat with Hayden and \<other person>", or "Nobody's talking
   to me in here, I'll wait for someone to ask me to do something."

5. **Integrated:** It has to come to you. Its interface should be wherever your
   company 'town square' is, such as Slack, Discord, Teams, etc. -- not in some
   third party "agentic chat" solution.

## Workspace Layout

```
crates/
    api/        The API service for the running gateway service.

    kernel/     The underlying domain-layer models & rules.

    ui/         The web frontend, built with Axum and React. The embedded SPA
                source is in ui/web.

    telemetry/  Tracing, metrics, and logging standards crate, providing thin,
                but opinionated telemetry helpers.
```

## Build/test/verify

Tasks live in `.mise/config.toml`; `mise tasks` lists them, `mise run <task>`
runs one. Mise is mandatory and pins the Rust toolchain plus required
components.

```
mise run build
mise run test                # hermetic tests: unit/serde/validation, no llm provider, no network
mise run clippy
mise run complexity-check    # cognitive complexity ratchet: offender count must not grow
mise run complexity          # advisory: list every function above the threshold
mise run fmt-check           # check formatting with rustfmt
```

Integration/E2E tests must **never** target the user's own resources.

## Working style here

- This is a phased build. Land one milestone, verify it (cargo test + clippy
  green), then start the next. Don't claim a milestone done without showing the
  passing test output.
- When a milestone is large and well-specified, it's fine to delegate to a
  subagent — but always independently re-run cargo test/clippy before trusting
  the result.
- Commit/push only when the user asks. Work happens on a feature or fix branch.
- Commits should follow [Conventional Commit](https://conventionalcommits.org)
