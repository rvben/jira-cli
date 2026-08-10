.PHONY: build release test test-e2e lint fmt check clean install release-patch release-minor release-major jira-start jira-stop jira-wait jira-logs jira-reset jira-backup jira-restore

build:
	cargo build

release:
	cargo build --release

# Every test target except e2e, which needs live Jira credentials. Selecting by
# exclusion means a new test file is covered the moment it is added; naming the
# included targets instead is how tests/cli.rs went unrun.
test:
	cargo nextest run -E 'not binary(e2e)'

# Run end-to-end tests against a real Jira instance.
# Requires: JIRA_E2E_HOST, JIRA_E2E_EMAIL, JIRA_E2E_TOKEN
# Optional: JIRA_E2E_PROJECT (default: TST)
test-e2e:
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
