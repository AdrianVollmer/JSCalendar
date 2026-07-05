# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder
WORKDIR /build

# Templates are embedded into the binary at compile time (Askama), so only
# the static/ assets need to ship in the runtime image, not templates/.
COPY Cargo.toml Cargo.lock ./
COPY crates/jmap-client crates/jmap-client
COPY crates/server crates/server
RUN cargo build --release --locked -p jscalendar-server

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 jscalendar

WORKDIR /app
COPY --from=builder /build/target/release/jscalendar-server /app/jscalendar-server
COPY crates/server/static /app/static

ENV JSCAL_STATIC_DIR=/app/static \
    PORT=8787 \
    JSCAL_TIMEZONE=UTC
EXPOSE 8787
USER jscalendar

ENTRYPOINT ["/app/jscalendar-server"]
