-include local.mk

IMAGE ?= shopping-sessions:latest
PLATFORM ?= linux/amd64
CPUS ?= 8
MEM ?= 24g
PYTHON ?= python3
FIXTURES ?= fixtures
ANSWER ?= dev-answer
DEV ?= dev
OUT ?= out
GENERATE ?= generate
export IMAGE PLATFORM CPUS MEM

.PHONY: build fixtures dev run-dev check-dev native fixtures-native run-dev-native
build:
	docker buildx build --platform $(PLATFORM) --load -t $(IMAGE) .
fixtures: build
	$(PYTHON) scripts/run_fixtures.py "$(FIXTURES)" -- scripts/docker_run.sh
dev:
	"$(GENERATE)" --seed 1 --events 1000000 --clicks 250000 --output "$(DEV)"
run-dev: build
	scripts/docker_run.sh "$(DEV)" "$(OUT)"
check-dev:
	$(PYTHON) scripts/compare.py "$(OUT)" "$(ANSWER)"
native:
	cargo build --release --locked
fixtures-native: native
	$(PYTHON) scripts/run_fixtures.py "$(FIXTURES)" -- target/release/shopping-sessions
run-dev-native: native
	DATA_DIR="$(DEV)" OUTPUT_DIR="$(OUT)" NCPU="$(CPUS)" target/release/shopping-sessions
