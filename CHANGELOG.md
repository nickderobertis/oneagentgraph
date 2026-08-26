# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.11](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.10...v0.3.11) - 2026-08-26

### Fixed

- *(deps)* adopt the published oneharness-core and onejudge ([#81](https://github.com/nickderobertis/oneagentgraph/pull/81))

## [0.3.10](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.9...v0.3.10) - 2026-08-25

### Fixed

- link the current oneharness-core and onejudge, and resolve one turn engine ([#79](https://github.com/nickderobertis/oneagentgraph/pull/79))

## [0.3.9](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.8...v0.3.9) - 2026-08-23

### Added

- run scripts before an agent's turn and inject their output into its context ([#77](https://github.com/nickderobertis/oneagentgraph/pull/77))

## [0.3.8](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.7...v0.3.8) - 2026-08-23

### Fixed

- stop the activity rule condemning a member while it writes its report ([#75](https://github.com/nickderobertis/oneagentgraph/pull/75))

## [0.3.7](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.6...v0.3.7) - 2026-08-23

### Fixed

- make the judged tier replay one verdict per tree, base and judge configuration ([#73](https://github.com/nickderobertis/oneagentgraph/pull/73))

## [0.3.6](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.5...v0.3.6) - 2026-08-21

### Fixed

- *(events)* publish tool results, live turn text and per-turn usage instead of dropping them ([#71](https://github.com/nickderobertis/oneagentgraph/pull/71))

## [0.3.5](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.4...v0.3.5) - 2026-08-20

### Fixed

- *(personas)* hold a dispatch to what it can prove from inside its own run ([#69](https://github.com/nickderobertis/oneagentgraph/pull/69))

## [0.3.4](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.3...v0.3.4) - 2026-08-19

### Fixed

- *(sweep)* stop a coverage report reading as a promise about the whole host ([#67](https://github.com/nickderobertis/oneagentgraph/pull/67))

## [0.3.3](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.2...v0.3.3) - 2026-08-19

### Added

- publish the conversation behind a member's turns ([#65](https://github.com/nickderobertis/oneagentgraph/pull/65))

## [0.3.2](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.1...v0.3.2) - 2026-08-19

### Fixed

- a member whose tree cannot be found is not a member proven idle ([#62](https://github.com/nickderobertis/oneagentgraph/pull/62))

## [0.3.1](https://github.com/nickderobertis/oneagentgraph/compare/v0.3.0...v0.3.1) - 2026-08-19

### Fixed

- leave a cancelled tree its grace before Windows ends the job ([#59](https://github.com/nickderobertis/oneagentgraph/pull/59))

## [0.3.0](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.19...v0.3.0) - 2026-08-18

### Added

- [**breaking**] make a persona a onejudge config fragment ([#54](https://github.com/nickderobertis/oneagentgraph/pull/54))

## [0.2.19](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.18...v0.2.19) - 2026-08-17

### Fixed

- anchor every written path through one helper, not three joins ([#52](https://github.com/nickderobertis/oneagentgraph/pull/52))

## [0.2.18](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.17...v0.2.18) - 2026-08-16

### Added

- drive a single-sided member's turn through the oneharness library ([#50](https://github.com/nickderobertis/oneagentgraph/pull/50))

## [0.2.16](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.15...v0.2.16) - 2026-08-16

### Fixed

- let a member's oneharness config decide its own run ([#46](https://github.com/nickderobertis/oneagentgraph/pull/46))

## [0.2.15](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.14...v0.2.15) - 2026-08-15

### Documentation

- write down the blocked oneharness-run boundary inventory, and pin the cwd invariant ([#44](https://github.com/nickderobertis/oneagentgraph/pull/44))

## [0.2.14](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.13...v0.2.14) - 2026-08-15

### Fixed

- *(persona)* dispatch repo-local personas and keep the shared bar ([#42](https://github.com/nickderobertis/oneagentgraph/pull/42))

## [0.2.13](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.12...v0.2.13) - 2026-08-15

### Added

- filter a run's merged event stream, from the graph or the CLI ([#40](https://github.com/nickderobertis/oneagentgraph/pull/40))

## [0.2.11](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.10...v0.2.11) - 2026-08-14

### Added

- compose a member task from the graph's, and defer a cron turn ([#37](https://github.com/nickderobertis/oneagentgraph/pull/37))

### Fixed

- judge a member's tree by a CPU rate, and serialize every graph fixture ([#34](https://github.com/nickderobertis/oneagentgraph/pull/34))

## [0.2.10](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.9...v0.2.10) - 2026-08-13

### Added

- scope a single-sided member's task, dir and liveness ([#32](https://github.com/nickderobertis/oneagentgraph/pull/32))

## [0.2.9](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.8...v0.2.9) - 2026-08-13

### Added

- expose liveness reset and interrupt as library calls ([#30](https://github.com/nickderobertis/oneagentgraph/pull/30))

## [0.2.8](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.7...v0.2.8) - 2026-08-13

### Added

- let a library caller start, watch and cancel a graph ([#27](https://github.com/nickderobertis/oneagentgraph/pull/27))

## [0.2.7](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.6...v0.2.7) - 2026-08-13

### Changed

- call oneharness as a library, drop the hand-rolled fake ([#24](https://github.com/nickderobertis/oneagentgraph/pull/24))

## [0.2.6](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.5...v0.2.6) - 2026-08-12

### Fixed

- let the smoke turn run on codex, and never block on stdin ([#22](https://github.com/nickderobertis/oneagentgraph/pull/22))

## [0.2.5](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.4...v0.2.5) - 2026-08-12

### Fixed

- make deps mean runs-only-if, and let cron drive chains ([#20](https://github.com/nickderobertis/oneagentgraph/pull/20))

## [0.2.4](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.3...v0.2.4) - 2026-08-12

### Fixed

- let --set populate an absent schema-known optional field ([#18](https://github.com/nickderobertis/oneagentgraph/pull/18))

## [0.2.3](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.2...v0.2.3) - 2026-08-12

### Added

- redirect a member's in-flight turn with an interrupt verb ([#15](https://github.com/nickderobertis/oneagentgraph/pull/15))

## [0.2.2](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.1...v0.2.2) - 2026-08-09

### Added

- expose scratch reclamation as an operator verb ([#13](https://github.com/nickderobertis/oneagentgraph/pull/13))

## [0.2.1](https://github.com/nickderobertis/oneagentgraph/compare/v0.2.0...v0.2.1) - 2026-08-09

### Added

- hold the liveness and scratch-ownership guarantees on Windows ([#5](https://github.com/nickderobertis/oneagentgraph/pull/5))

## [0.2.0](https://github.com/nickderobertis/oneagentgraph/compare/v0.1.1...v0.2.0) - 2026-08-09

### Added

- [**breaking**] drive onejudge as a library instead of a CLI subprocess ([#9](https://github.com/nickderobertis/oneagentgraph/pull/9))

## [0.1.1](https://github.com/nickderobertis/oneagentgraph/compare/v0.1.0...v0.1.1) - 2026-08-08

### Added

- port e2e suite and implement the oneagentgraph contract ([#3](https://github.com/nickderobertis/oneagentgraph/pull/3))

## [0.1.0](https://github.com/nickderobertis/oneagentgraph/releases/tag/v0.1.0) - 2026-08-08

### Added

- bootstrap the repo and lay the contract down interface-only

### Fixed

- close the llmlint buildout findings
- close the llmlint findings the first full-tree run surfaced
# Changelog

All notable changes to this project are documented here.

This file is maintained by [release-plz](https://release-plz.dev/) from
Conventional Commits — do not edit it by hand.
