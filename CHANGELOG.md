# Changelog

This file records the notable changes to deadband. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- A `deadband run` whose daemon goes away connects again every 100 ms and
  tells the next daemon that its job is already running. The new daemon
  counts that job at once, so a restart no longer admits a second full set
  of jobs on top of the ones that run.
