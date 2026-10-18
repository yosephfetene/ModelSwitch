# Architecture Decision Records

## ADR-001 --- Modular monolith first

**Status:** Accepted for initial release.

**Context:** The project needs clean boundaries without microservice
deployment overhead.

**Decision:** One Rust service with separate API, domain, routing,
provider, reliability, accounting, and storage modules.

**Consequences:** Easier local development and integration testing;
module dependency direction must stay clear.

## ADR-002 --- Explicit routing before adaptive routing

**Status:** Accepted.

**Context:** Adaptive routing needs representative data and a defined
objective.

**Decision:** Begin with deterministic, versioned rules. Add
benchmark-driven routing after a repeatable workload exists.

**Consequences:** Initial behavior is explainable and testable; adaptive
routing is deferred.

## ADR-003 --- Separate requests from attempts

**Status:** Accepted.

**Context:** A logical request can create multiple upstream attempts.

**Decision:** Store one request record and individual attempt records.

**Consequences:** More accurate failure/cost analysis, with somewhat
more persistence logic.

## ADR-004 --- Label estimated cost

**Status:** Accepted.

**Context:** Usage and prices vary across providers and change over
time.

**Decision:** Keep reported usage distinct from estimated cost and
record the pricing version.

**Consequences:** Historical estimates are explainable; pricing data
needs maintenance.

## ADR-005 --- No content logging by default

**Status:** Accepted.

**Context:** Prompts and completions may contain sensitive data.

**Decision:** Log metadata, timing, usage, and outcomes by default, not
prompt/completion content.

**Consequences:** Debugging should use safe fixtures or explicitly
enabled time-limited diagnostic capture.

## ADR-006 --- Non-streaming text generation first

**Status:** Accepted for first vertical slice.

**Context:** Streaming and tool calls introduce more protocol/capability
complexity.

**Decision:** Prove request, provider, persistence, and error paths
before adding them.

**Consequences:** Smaller first milestone; capability expansion requires
deliberate API evolution.
