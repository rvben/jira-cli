.PHONY: build release nextest test test-e2e lint fmt check clean install release-patch release-minor release-major jira-start jira-stop jira-wait jira-logs jira-reset jira-backup jira-restore

build:
	cargo build

release:
	cargo build --release

# The test runner. Fail with the install command rather than cargo's
# "no such command" when it is missing. Tests that spawn the binary find it
# through CARGO_BIN_EXE_jira at runtime, because this toolchain builds test
# binaries away from target/debug; nextest sets that variable from 0.9.130 on,
# and under an older one every such test panics.
NEXTEST_MIN := 0.9.130
nextest:
	@v=$$(cargo nextest --version 2>/dev/null | awk 'NR == 1 { print $$2 }'); \
	if [ -z "$$v" ]; then \
		echo "cargo-nextest is not installed; install it with: cargo install cargo-nextest --locked" >&2; \
		exit 1; \
	fi; \
	if [ "$$(printf '%s\n%s\n' "$(NEXTEST_MIN)" "$$v" | sort -V | head -n 1)" != "$(NEXTEST_MIN)" ]; then \
		echo "cargo-nextest $$v is older than $(NEXTEST_MIN), the first release that sets CARGO_BIN_EXE_jira for these tests; upgrade with: cargo install cargo-nextest --locked" >&2; \
		exit 1; \
	fi

# Every test target except e2e, which needs live Jira credentials. Selecting by
# exclusion means a new test file is covered the moment it is added; naming the
# included targets instead is how tests/cli.rs went unrun.
test: nextest
	cargo nextest run -E 'not binary(e2e)'

# Run end-to-end tests against a real Jira instance.
# Requires: JIRA_E2E_HOST, JIRA_E2E_EMAIL, JIRA_E2E_TOKEN
# Optional: JIRA_E2E_PROJECT (default: TST)
test-e2e: nextest
	cargo nextest run --test e2e

# --all-targets so test code is linted too. Without it clippy sees only the
# library and the binary, and the test suite goes unchecked.
lint:
	cargo fmt -- --check
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt

check: lint test

clean:
	cargo clean

install: check release
	cp target/release/jira ~/.local/bin/jira

publish:
	cargo publish

# ── Local Jira (Data Center) for integration testing ──────────────────────────
jira-start:
	docker compose -f docker/docker-compose.yml up -d

jira-stop:
	docker compose -f docker/docker-compose.yml down

jira-wait:
	docker/wait-for-jira.sh

jira-logs:
	docker compose -f docker/docker-compose.yml logs -f jira

jira-reset:
	docker compose -f docker/docker-compose.yml down -v

jira-backup:
	docker compose -f docker/docker-compose.yml stop
	mkdir -p docker/backup
	docker run --rm -v docker_jira-data:/data -v $(CURDIR)/docker/backup:/backup busybox tar czf /backup/jira-data.tar.gz -C /data .
	docker run --rm -v docker_postgres-data:/data -v $(CURDIR)/docker/backup:/backup busybox tar czf /backup/postgres-data.tar.gz -C /data .
	docker compose -f docker/docker-compose.yml start
	@echo "Backup written to docker/backup/"

jira-restore:
	docker compose -f docker/docker-compose.yml down -v
	docker volume create docker_jira-data
	docker volume create docker_postgres-data
	docker run --rm -v docker_jira-data:/data -v $(CURDIR)/docker/backup:/backup busybox tar xzf /backup/jira-data.tar.gz -C /data
	docker run --rm -v docker_postgres-data:/data -v $(CURDIR)/docker/backup:/backup busybox tar xzf /backup/postgres-data.tar.gz -C /data
	docker compose -f docker/docker-compose.yml up -d
	@echo "Restore complete — run 'make jira-wait' to confirm readiness"

release-patch:
	vership bump patch

release-minor:
	vership bump minor

release-major:
	vership bump major
