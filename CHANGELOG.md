# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [0.4.1](https://github.com/rvben/jira-cli/compare/v0.4.0...v0.4.1) - 2026-08-10

**`JIRA_READ_ONLY` could silently fail open.** It matched `1`, `true`, `yes` and
`on` exactly, so `JIRA_READ_ONLY=True` resolved to "off" and every write went
through while the guard looked enabled. Values are now matched
case-insensitively, and anything that is neither an on nor an off value is
rejected as a config error instead of read as "off".

`JIRA_AUTH_TYPE` and `JIRA_API_VERSION` had the same shape of bug: a typo fell
back to basic auth (surfacing as an opaque 401 from Jira), and an unsupported
version built requests against `/rest/api/<n>/` and reported Jira's 404 as the
problem. Both now reject an unrecognised value and name what they accept.

If you rely on one of those values being accepted loosely, the command now
exits 2 rather than continuing with a default. `JIRA_DEBUG_HTTP` is unchanged in
this respect: a typo there is still read as off, because failing a command over
a debug switch costs more than the missed logging.

### Added

- **schema**: declare read-only mode and diagnostics in the schema ([8b37ab3](https://github.com/rvben/jira-cli/commit/8b37ab39a8d51181e8d0ca3db5bb379c94debddf))

### Fixed

- **config**: reject unrecognised environment values instead of defaulting ([af952bb](https://github.com/rvben/jira-cli/commit/af952bb35205363712d2136f44df6df89bfebef0))
- **schema**: show auth_type and api_version in the example config ([8e781f2](https://github.com/rvben/jira-cli/commit/8e781f27125f2a7848914509e6da44c0cdd6519f))

## [0.4.0](https://github.com/rvben/jira-cli/compare/v0.3.16...v0.4.0) - 2026-08-10

Attachment operations were contributed by [@phlppbmm](https://github.com/phlppbmm)
in [#6](https://github.com/rvben/jira-cli/pull/6). Thank you.

**Breaking, JSON output only.** `assignee`, `priority`, `reporter` and
`description` now report `null` when the field is absent. They previously
reported `"-"`, the placeholder meant for table output, which made an unassigned
issue indistinguishable from one assigned to a user whose display name is `-`
(an empty description was `""`). `issues assign` now emits `accountId: null` when
it unassigns, instead of an undeclared `assignee` key, so both paths return the
same keys. Table output is unchanged.

### Added

- **issues**: add attachment operations ([65d1a29](https://github.com/rvben/jira-cli/commit/65d1a290b08422fa0c2d44dbfe7d72a6522234c4))

### Fixed

- **schema**: declare the fields each command actually emits ([cdb43aa](https://github.com/rvben/jira-cli/commit/cdb43aac4dee30a5258e122f650af86d433861f4))
- **issues**: report an absent field as null in JSON, not "-" ([8006386](https://github.com/rvben/jira-cli/commit/80063864c23bdb555d44334f1d608cff7ce7ab40))
- **issues**: report an absent attachment author as null, not "-" ([6bf201d](https://github.com/rvben/jira-cli/commit/6bf201da88de994625dd063c674d98806344862e))
- **issues**: add --force flag and pre-flight checks to download-attachment ([8398fd1](https://github.com/rvben/jira-cli/commit/8398fd19933587eb9e963fc335612913e56fd4db))

## [0.3.16](https://github.com/rvben/jira-cli/compare/v0.3.15...v0.3.16) - 2026-08-09

### Fixed

- **errors**: make declared error kinds reachable and stderr single-format ([400250b](https://github.com/rvben/jira-cli/commit/400250bb725ada3338da3d0905549c764f5ccfed))
- emit the JSON output and error kinds that schema declares ([5435ed1](https://github.com/rvben/jira-cli/commit/5435ed1d56700fc57a94116c28437dc6eeacd3e7))

## [0.3.15](https://github.com/rvben/jira-cli/compare/v0.3.14...v0.3.15) - 2026-06-20

### Fixed

- **schema**: correct exit-code declarations ([55711d9](https://github.com/rvben/jira-cli/commit/55711d985ace403b47785b522022c5f6f529cb51))

## [0.3.14](https://github.com/rvben/jira-cli/compare/v0.3.13...v0.3.14) - 2026-06-11

### Added

- comply with CLI Spec v0.2 (schema, output, error envelope) ([ba3dad3](https://github.com/rvben/jira-cli/commit/ba3dad3bb65c914f2a852848022c2fa877443576))

## [0.3.13](https://github.com/rvben/jira-cli/compare/v0.3.12...v0.3.13) - 2026-05-02

### Added

- **projects**: add versions subcommand for version discovery ([7d46727](https://github.com/rvben/jira-cli/commit/7d46727a299bad1c4f8f38b97792bfb0a986b336))
- **issues**: add --fix-versions filter to issues list ([ada429b](https://github.com/rvben/jira-cli/commit/ada429b502a8003e961c98ed90fc39171319b763))
- **issues**: add --fix-versions, --labels, --assignee flags to issues update ([06d61d3](https://github.com/rvben/jira-cli/commit/06d61d3f5abc0f5f21037fed3188aca8884e2bfb))
- **issues**: add --fix-version flag to issues create ([4ddd0f6](https://github.com/rvben/jira-cli/commit/4ddd0f61fd11cc83f035aee9332a5eda67384d13))
- **issues**: render fix versions and affected versions in issue detail view ([3f00bf2](https://github.com/rvben/jira-cli/commit/3f00bf203b27b313d7221c6393c1226a846e3b45))
- **issues**: deserialize fixVersions and affectedVersions from Jira API ([8f285b5](https://github.com/rvben/jira-cli/commit/8f285b56bd947164e5af12cf29f1e46947b5ce4f))

## [0.3.12](https://github.com/rvben/jira-cli/compare/v0.3.11...v0.3.12) - 2026-04-28

### Added

- **issues**: add --labels filter to issues list ([3956f89](https://github.com/rvben/jira-cli/commit/3956f89e6706f21f86d5068bd0983807d53ed0ad))
- **issues**: include components in issues show --json output ([eed7328](https://github.com/rvben/jira-cli/commit/eed73281bf1f47de51d807bb404c9ce0bc7a7daa))
- **projects**: add components subcommand for component discovery ([f97c012](https://github.com/rvben/jira-cli/commit/f97c0120b2f41dc650ac2dd4f9b4aaf745822df6))
- **issues**: add --components filter to issues list ([8c6c4a7](https://github.com/rvben/jira-cli/commit/8c6c4a75d4f1b57fc6a420600d79b3a7ed74497b))
- **issues**: add --components flag to issues update ([4c856cb](https://github.com/rvben/jira-cli/commit/4c856cb5acc5ccfbd7ac513b4f171a4d37c73873))
- **issues**: add --component flag to issues create ([4daa8e5](https://github.com/rvben/jira-cli/commit/4daa8e518508fab59c18782b14ebc221aa358ab4))
- **issues**: render components in issue detail view ([d3d40c5](https://github.com/rvben/jira-cli/commit/d3d40c53180646185eea9e50a07301c6535116d2))
- **issues**: deserialize component field from Jira API ([232bc93](https://github.com/rvben/jira-cli/commit/232bc93de5d21d584f22762011dd92feea04d560))

## [0.3.11](https://github.com/rvben/jira-cli/compare/v0.3.10...v0.3.11) - 2026-04-23

### Added

- **issues**: detect real terminal width for the issues table ([7e44c00](https://github.com/rvben/jira-cli/commit/7e44c00359749e51743c28db37c97203cb5afd07))

### Fixed

- **errors**: surface Jira errorMessages in default error summary ([e7645a1](https://github.com/rvben/jira-cli/commit/e7645a1f61f94fb32297a0c7121dde0981a4addc))

## [0.3.10](https://github.com/rvben/jira-cli/compare/v0.3.9...v0.3.10) - 2026-04-23

### Fixed

- **search**: harden v3 cursor walk and clean up search path ([3643291](https://github.com/rvben/jira-cli/commit/36432911a899cc9fe86efdd75e98a180a3d402cb))
- migrate Jira Cloud search to /rest/api/3/search/jql ([c858fa2](https://github.com/rvben/jira-cli/commit/c858fa251b437e274568680047adb584a8eacf34))

## [0.3.9](https://github.com/rvben/jira-cli/compare/v0.3.8...v0.3.9) - 2026-04-08

### Added

- publish to PyPI as jira-cli-rs ([8f5370a](https://github.com/rvben/jira-cli/commit/8f5370a26bd9b162be95c7f5e78a47a6771fd9a8))

## [0.3.8](https://github.com/rvben/jira-cli/compare/v0.3.7...v0.3.8) - 2026-04-07

### Added

- `jira issue PROJ-123` falls through to `issues show` ([3764b1f](https://github.com/rvben/jira-cli/commit/3764b1f746a60bd853677b7d38a32d014002c5fe))
- add singular aliases for all subcommand groups ([2d05eea](https://github.com/rvben/jira-cli/commit/2d05eea6fa74ae5ee1ddadf169a54afa43d1490d))

### Fixed

- schema tests acquire env lock to prevent XDG_CONFIG_HOME leakage ([204b794](https://github.com/rvben/jira-cli/commit/204b79422741328a81be0b70744a8a9078e8eb4b))

## [0.3.7](https://github.com/rvben/jira-cli/compare/v0.3.6...v0.3.7) - 2026-04-03

### Added

- add top-level `issue` command as shortcut for `issues show` ([788bcc4](https://github.com/rvben/jira-cli/commit/788bcc4722b5a23d1fb11d08fdfabf814e2c53f5))

## [0.3.6](https://github.com/rvben/jira-cli/compare/v0.3.5...v0.3.6) - 2026-04-03

## [0.3.5](https://github.com/rvben/jira-cli/compare/v0.3.4...v0.3.5) - 2026-04-03

## [0.3.4](https://github.com/rvben/jira-cli/compare/v0.3.3...v0.3.4) - 2026-04-01

### Added

- add read-only mode via JIRA_READ_ONLY env var and config field ([68e15a3](https://github.com/rvben/jira-cli/commit/68e15a353c5000488516ccc929597c6da5df7929))

## [0.3.3](https://github.com/rvben/jira-cli/compare/v0.3.2...v0.3.3) - 2026-03-31

### Fixed

- **config**: show token in plain text during init ([fd62572](https://github.com/rvben/jira-cli/commit/fd6257201119664fa280e0d3b8d30983450cac23))

## [0.3.2](https://github.com/rvben/jira-cli/compare/v0.3.1...v0.3.2) - 2026-03-31

### Added

- **config**: interactive init wizard and profile removal ([1db53db](https://github.com/rvben/jira-cli/commit/1db53dbaf75c65d0d0ae3fcde9de6e3b878ed8a8))

## [0.3.1](https://github.com/rvben/jira-cli/compare/v0.3.0...v0.3.1) - 2026-03-30

### Fixed

- simplify mount_board_and_sprints to async fn per clippy lint ([37e094b](https://github.com/rvben/jira-cli/commit/37e094b6fe2c1c6f8602c3faccaea1d8adcfbb73))

## [0.3.0](https://github.com/rvben/jira-cli/compare/v0.2.0...v0.3.0) - 2026-03-30

### Added

- **issues**: add worklog, bulk ops, and subtask support ([5383672](https://github.com/rvben/jira-cli/commit/53836728887f079934ed793a7be96665e9b152be))

## [0.2.0](https://github.com/rvben/jira-cli/compare/v0.1.0...v0.2.0) - 2026-03-30

### Added

- **issues**: add --all pagination, issues mine, and issues comments ([725def7](https://github.com/rvben/jira-cli/commit/725def78a7580e43a27473951ece76024050b82a))
- add users, boards, sprints, fields, issue links, and sprint assignment ([639fb26](https://github.com/rvben/jira-cli/commit/639fb2641a6ab744c66204f1b305c6e7b402b65d))
- improve config init output with DC/Server PAT instructions ([0193584](https://github.com/rvben/jira-cli/commit/01935847c8e02c50fba48864af9cc6edb554b2ce))
- add Jira Data Center / Server support ([f654ef3](https://github.com/rvben/jira-cli/commit/f654ef3c399f54b326ee0cdafe085caafd4b8327))

## [0.1.0](https://github.com/rvben/jira-cli/compare/v0.0.2...v0.1.0) - 2026-03-30

## [0.0.2] - 2026-03-30

### Added

- initial release of jira CLI ([e5f730b](https://github.com/rvben/jira-cli/commit/e5f730ba424a2b753d333fa389f0c3491d6f6402))

### Fixed

- align config bootstrap and schema contract ([a316125](https://github.com/rvben/jira-cli/commit/a316125cb243e209ecacf59af96980fbb4eace21))
- harden jira api behavior and pagination ([64956bf](https://github.com/rvben/jira-cli/commit/64956bfe702f094002d65cf476ddc01175283245))
