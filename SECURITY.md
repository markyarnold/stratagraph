# Security Policy

## Reporting a vulnerability

If you find a security vulnerability in StrataGraph, please report it privately.
**Do not open a public GitHub issue for security problems.**

Email **mark@thecloudlabs.uk** with:

- a description of the issue and its impact,
- the version (`strata --version`) and your platform, and
- steps to reproduce, ideally a minimal proof of concept.

We will acknowledge your report, keep you posted on the fix, and credit you in the
release notes if you would like. Please give us a reasonable window to ship a fix
before any public disclosure.

## Supported versions

StrataGraph is in active development ahead of a 1.0 release. Security fixes land on
the latest release and on `develop`; please reproduce against a recent build
before reporting.

## Scope and trust model

StrataGraph runs locally as a single binary and indexes your code on your own
machine. The deterministic engine makes no network calls: it reads your source and
writes a local `.strata/` index. The optional model pass, when used, transmits only
what you explicitly opt into, under your own provider key. Reports about the engine,
the CLI, the MCP server, the desktop app, and the agent-kit installer are all in
scope.
