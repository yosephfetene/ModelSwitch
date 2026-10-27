# Build stage: compile a release binary.
FROM rust:1.96-slim-trixie AS build
WORKDIR /app

# Build dependencies first so they are cached until Cargo.toml or
# Cargo.lock change, not on every source edit.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

COPY src ./src
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

ENV MODELSWITCH_CONFIG=/etc/modelswitch/modelswitch.toml
USER modelswitch
EXPOSE 8080

# DATABASE_URL must be provided at runtime; it is never baked in.
ENTRYPOINT ["modelswitch"]
CMD ["serve"]
