# Changelog

All notable changes to Harpas are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Harpas adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) as described in [docs/releases.md](docs/releases.md).

## [Unreleased]

### Added

- Embedded JDK 25 bridge benchmark with JNI/FFM calls, shared player views, synchronous events for 200
  simulated players, and behavioral checks for both dispatch paths (`harpas-bench`).
- Java 26.1 Handshaking, Status and Login packet codecs, generated from Harpas' own protocol schema
  (`harpas-protocol-java`), with a check of the schema against minecraft-data (`harpas-protocol-codegen`).

### Changed

### Fixed

### Removed

### Security
