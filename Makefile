CONTAINER_ENGINE ?= $(shell command -v podman >/dev/null 2>&1 && echo podman || echo docker)
IMAGE            ?= jscalendar
TAG              ?= latest
PORT             ?= 8787

.PHONY: help build test clippy fmt fmt-check run clean \
        docker-build docker-run docker-push

help:
	@echo "Native (needs a Rust toolchain):"
	@echo "  make build         cargo build --release"
	@echo "  make test          cargo test --workspace"
	@echo "  make clippy        cargo clippy --workspace --all-targets"
	@echo "  make fmt           cargo fmt --all"
	@echo "  make run           run the server on \$$PORT (default $(PORT))"
	@echo "  make clean         cargo clean"
	@echo ""
	@echo "Containerized (uses podman if installed, else docker; override with CONTAINER_ENGINE=):"
	@echo "  make docker-build  build the '$(IMAGE):$(TAG)' image"
	@echo "  make docker-run    run the image on \$$PORT (default $(PORT))"

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

clean:
	cargo clean

docker-build:
	$(CONTAINER_ENGINE) build -t $(IMAGE):$(TAG) .

docker-run: docker-build
	$(CONTAINER_ENGINE) run --rm -it -p $(PORT):8787 -e PORT=8787 $(IMAGE):$(TAG)
