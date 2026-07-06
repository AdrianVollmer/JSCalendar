#!/bin/sh
# Runs the mock JMAP server (internal-only, on 127.0.0.1:9090) alongside the
# real app server (on $PORT, exposed) in one container, for a zero-setup
# demo/test instance. Only the app server's port needs to be published.
set -e

PORT=9090 BIND_ADDR=127.0.0.1 /app/mock-jmap-server &
MOCK_PID=$!

/app/jscalendar-server &
APP_PID=$!

trap 'kill "$MOCK_PID" "$APP_PID" 2>/dev/null' TERM INT

wait "$APP_PID"
kill "$MOCK_PID" 2>/dev/null || true
