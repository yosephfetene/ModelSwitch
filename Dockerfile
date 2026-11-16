# Dashboard stage: build the page into plain files (ADR-036). Node lives
# only here; the runtime image holds just the output.
FROM node:24.22-trixie-slim AS dashboard
WORKDIR /dashboard
# Install from the lockfile first so dependencies are cached until it
# changes, not on every source edit.
COPY dashboard/package.json dashboard/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY dashboard/ ./
RUN npm run build

# Build stage: compile a release binary.
FROM rust:1.96-slim-trixie AS build
WORKDIR /app

# Build dependencies first so they are cached until Cargo.toml or
# Cargo.lock change, not on every source edit.
COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src ./src
# Embedded into the binary by `sqlx::migrate!`.
COPY migrations ./migrations
# Make sure cargo rebuilds the real sources rather than the stubs.
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

# Runtime stage: a small image with only the binary and its config.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home modelswitch

COPY --from=build /app/target/release/modelswitch /usr/local/bin/modelswitch
COPY deploy/modelswitch.docker.toml /etc/modelswitch/modelswitch.toml
# The same, plus the dashboard on the admin port. Not the default: it
# needs MODELSWITCH_ADMIN_TOKEN, and its port must be published to the
# host's loopback only (see docker-compose.yml).
COPY deploy/modelswitch.docker-admin.toml /etc/modelswitch/modelswitch-admin.toml
COPY --from=dashboard /dashboard/dist /usr/share/modelswitch/dashboard

ENV MODELSWITCH_CONFIG=/etc/modelswitch/modelswitch.toml
USER modelswitch
# 8080: the public API. 9091: the admin port, only with the admin config.
EXPOSE 8080 9091

# DATABASE_URL must be provided at runtime; it is never baked in.
ENTRYPOINT ["modelswitch"]
CMD ["serve"]
