CONTAINER_ENGINE ?= $(shell command -v podman >/dev/null 2>&1 && echo podman || echo docker)
IMAGE            ?= jscalendar
DEMO_IMAGE       ?= jscalendar-demo
TAG              ?= latest
PORT             ?= 8787
MOCK_PORT        ?= 9090

.PHONY: help build test clippy fmt fmt-check run mock-server demo clean \
        docker-build docker-run docker-demo-build docker-demo-run

help:
	@echo "Native (needs a Rust toolchain):"
	@echo "  make build          cargo build --release"
	@echo "  make test           cargo test --workspace"
	@echo "  make clippy         cargo clippy --workspace --all-targets"
	@echo "  make fmt            cargo fmt --all"
	@echo "  make run            run the server on \$$PORT (default $(PORT)) against a real JMAP account"
	@echo "  make mock-server    run just the in-memory mock JMAP server on \$$MOCK_PORT (default $(MOCK_PORT))"
	@echo "  make demo           run the app + mock JMAP server together, login form pre-filled"
	@echo "  make clean          cargo clean"
	@echo ""
	@echo "Containerized (uses podman if installed, else docker; override with CONTAINER_ENGINE=):"
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
	$(CONTAINER_ENGINE) run --rm -it -p $(PORT):8787 -e PORT=8787 $(IMAGE):$(TAG)

docker-demo-build:
	$(CONTAINER_ENGINE) build -f Dockerfile.demo -t $(DEMO_IMAGE):$(TAG) .

docker-demo-run: docker-demo-build
	$(CONTAINER_ENGINE) run --rm -it -p $(PORT):8787 -e PORT=8787 $(DEMO_IMAGE):$(TAG)
