# Contributing to StrataGraph

Thanks for your interest. This document covers the license your contributions are
made under, the sign-off we require, and the workflow.

## License of contributions

StrataGraph is open source under the Apache License 2.0 (see [LICENSE.md](LICENSE.md)).
By contributing, you agree that your contributions are licensed under the same terms.
We use the DCO (below) rather than a CLA, so you retain copyright in your contribution.

## Developer Certificate of Origin (DCO)

We use the [Developer Certificate of Origin 1.1](https://developercertificate.org/),
a lightweight statement that you wrote the patch or otherwise have the right to
submit it under the project's license. Sign off every commit:

```bash
git commit -s -m "your message"
```

This appends a `Signed-off-by: Your Name <you@example.com>` line. Commits without
a sign-off cannot be merged.

## Workflow

- Branch off `develop`; open pull requests against `develop`.
- Keep changes focused: one logical change per PR.
- The build gate must pass, all green: `cargo test`,
  `cargo clippy -- -D warnings`, and `cargo fmt --check`.
- New behavior needs tests. Accuracy-affecting changes update the relevant report
  under `docs/accuracy/`.
- Write commit messages that explain the why, not just the what.

## What lives in this repository

This repository is the **full StrataGraph engine and suite**: the CLI, the MCP
server, the desktop app, the agent kit, and multi-repo estates, all open source.
Roadmap capabilities (org-scale hosted estates, history, collaboration, governance)
will land here too, also open source. A managed/hosted service may be offered
commercially in future; see [docs/commercialisation/](docs/commercialisation/README.md).
That would sell operation, not code: everything in this repository stays Apache-2.0.

## Code of conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By taking
part you are expected to uphold it; report unacceptable behaviour to
mark@thecloudlabs.uk.

## Reporting issues

Open an issue with a minimal reproduction and your `strata --version` (which
prints the engine id), so a result can be tied to the exact build that produced
it.
