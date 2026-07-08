CONTAINER_ENGINE ?= $(shell command -v podman >/dev/null 2>&1 && echo podman || echo docker)
IMAGE            ?= jscalendar
DEMO_IMAGE       ?= jscalendar-demo
TAG              ?= latest
PORT             ?= 8787
MOCK_PORT        ?= 9090

# Used by the docker-* dev targets (test/clippy/fmt run in a container
# instead of needing a local Rust toolchain). Named volumes cache the
# registry and build artifacts across runs so repeat invocations are fast;
# they're separate from ./target so a containerized build never collides
# with a native one built on a different base image.
CARGO_IMAGE          ?= rust:1-bookworm
CARGO_REGISTRY_CACHE ?= jscalendar-cargo-registry
CARGO_TARGET_CACHE   ?= jscalendar-cargo-target
DOCKER_CARGO = $(CONTAINER_ENGINE) run --rm \
	-v "$(CURDIR)":/work -w /work \
	-v $(CARGO_REGISTRY_CACHE):/usr/local/cargo/registry \
	-v $(CARGO_TARGET_CACHE):/work/target \
	$(CARGO_IMAGE)

.PHONY: help build test clippy fmt fmt-check run mock-server demo clean \
        docker-build docker-run docker-demo-build docker-demo-run \
        docker-test docker-clippy docker-fmt docker-fmt-check docker-check

help:
	@echo "Native (needs a Rust toolchain):"
	@echo "  make build          cargo build --release"
	@echo "  make test           cargo test --workspace"
	@echo "  make clippy         cargo clippy --workspace --all-targets"
	@echo "  make fmt            cargo fmt --all"
	@echo "  make fmt-check      cargo fmt --all -- --check"
	@echo "  make run            run the server on \$$PORT (default $(PORT)) against a real JMAP account"
	@echo "  make mock-server    run just the in-memory mock JMAP server on \$$MOCK_PORT (default $(MOCK_PORT))"
	@echo "  make demo           run the app + mock JMAP server together, login form pre-filled"
	@echo "  make clean          cargo clean"
	@echo ""
	@echo "Same checks, containerized (no local Rust toolchain needed; uses podman if"
	@echo "installed, else docker — override with CONTAINER_ENGINE=):"
	@echo "  make docker-test        cargo test --workspace, in a container"
	@echo "  make docker-clippy      cargo clippy --workspace --all-targets -D warnings, in a container"
	@echo "  make docker-fmt         cargo fmt --all, in a container (writes back to your files)"
	@echo "  make docker-fmt-check   cargo fmt --all -- --check, in a container"
	@echo "  make docker-check       fmt-check + clippy + test, in one container run"
	@echo ""
	@echo "Containerized app image (uses podman if installed, else docker; override with CONTAINER_ENGINE=):"
	@echo "  make docker-build       build the production '$(IMAGE):$(TAG)' image"
	@echo "  make docker-run         run the production image on \$$PORT (default $(PORT))"
	@echo "  make docker-demo-build  build the '$(DEMO_IMAGE):$(TAG)' image (app + mock server + seed data)"
	@echo "  make docker-demo-run    run it on \$$PORT (default $(PORT)) — no JMAP account needed"

build:
	cargo build --release --workspace

test:
	cargo test --workspace

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

docker-test:
	$(DOCKER_CARGO) cargo test --workspace

docker-clippy:
	$(DOCKER_CARGO) cargo clippy --workspace --all-targets -- -D warnings

docker-fmt:
	$(DOCKER_CARGO) cargo fmt --all

docker-fmt-check:
	$(DOCKER_CARGO) cargo fmt --all -- --check

docker-check: docker-fmt-check docker-clippy docker-test

run:
	PORT=$(PORT) cargo run -p jscalendar-server

mock-server:
	PORT=$(MOCK_PORT) cargo run -p mock-jmap-server

## Runs the mock JMAP server in the background and the app in the
## foreground, with the login form pre-filled so signing in is one click.
## Ctrl-C stops both.
demo:
	@trap 'kill 0' EXIT INT TERM; \
	PORT=$(MOCK_PORT) BIND_ADDR=127.0.0.1 cargo run -p mock-jmap-server & \
	sleep 1; \
	echo "Demo ready: open http://localhost:$(PORT)/login (already pre-filled) — Ctrl-C to stop"; \
	JSCAL_DEMO_SERVER_URL=http://127.0.0.1:$(MOCK_PORT) \
	JSCAL_DEMO_USERNAME=demo \
	JSCAL_DEMO_PASSWORD=demo \
	PORT=$(PORT) cargo run -p jscalendar-server

clean:
	cargo clean

docker-build:
	$(CONTAINER_ENGINE) build -t $(IMAGE):$(TAG) .

docker-run: docker-build
	$(CONTAINER_ENGINE) run --rm -it --init -p $(PORT):8787 -e PORT=8787 $(IMAGE):$(TAG)

docker-demo-build:
	$(CONTAINER_ENGINE) build -f Dockerfile.demo -t $(DEMO_IMAGE):$(TAG) .

docker-demo-run: docker-demo-build
	$(CONTAINER_ENGINE) run --rm -it --init -p $(PORT):8787 -e PORT=8787 $(DEMO_IMAGE):$(TAG)
