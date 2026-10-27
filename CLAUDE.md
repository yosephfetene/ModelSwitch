# CLAUDE.md

ModelSwitch is a Rust LLM inference gateway: one API over several
providers, with explicit routing, bounded failover, and usage accounting.
Read `docs/` before designing anything; `docs/DECISIONS.md` records why
things are the way they are. Do not contradict an accepted ADR without
proposing a new one.

## Hard rules

- Never log prompts, completions, API keys, or connection strings.
  Secrets are `secrecy::SecretString`.
- Every upstream call (providers, database) runs under a deadline.
- Retries and fallback are finite and recorded per attempt.
- Missing token usage stays `null` / `unavailable`; never estimate it into
  a "reported" field (ADR-010).
- Errors reach HTTP only through the single `ApiError` mapping; responses
  never contain raw provider bodies, stack traces, or secrets (ADR-019).
- Health endpoints need no API key; readiness checks only PostgreSQL
  (ADR-021).

## Architecture

- One crate: `src/lib.rs` holds the app, `src/main.rs` only wires it up
  (ADR-017). Integration tests build the router in-process.
- Domain, routing, and reliability code must not depend on `axum` or
  `sqlx`.
- External dependencies (database, providers, clock, environment) are
  reached through traits or injected parameters so tests can substitute
  fakes. Do not read `std::env` or call real services from library code
  that tests exercise.
- Structure comes from `modelswitch.toml`; secrets come from environment
  variables that the file names (ADR-018).

## Workflow

- Behavior is specified as test cases before implementation.
- Work in small slices: one branch and one pull request each.
- The AI writes code and runs the checks below before handing over. The
  human reviews the diff, runs all git and `gh` commands, and merges
  (squash). Do not commit, push, or open pull requests unless asked.
- New design decisions become ADRs in `docs/DECISIONS.md`; update the
  affected docs in the same pull request.
- Commit messages and PR titles follow Conventional Commits.

## Checks

``` bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test            # needs DATABASE_URL (see .env.example)
cargo deny check      # vulnerabilities, licenses, sources
```

CI runs all of these plus a Docker build and a gitleaks secret scan.
New dependencies must use a license in the `deny.toml` allowlist.

## Local notes

- Database passwords: letters and digits only (`DATABASE_URL` is a URL).
- On Windows PowerShell use `curl.exe`, not the `curl` alias.
- A running `modelswitch.exe` locks `target/debug`; stop the server
  before rebuilding, or build with a separate `--target-dir`.
