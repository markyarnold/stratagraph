# StrataGraph: license and the (possible) hosted option

StrataGraph is **open source under the Apache License 2.0** ([../../LICENSE.md](../../LICENSE.md)).
The entire suite is in this repository and free to use, modify, self-host, and
redistribute: the `strata` CLI and all its tools, the MCP server, the desktop app,
the agent kit, and multi-repo estates. The org-scale capabilities still on the
roadmap (hosted estates at scale, history and trends, team collaboration, PR/CI
governance, SSO/RBAC/audit) will land here too, also Apache-2.0. Nothing is paywalled.

## The (possible) commercial layer

A managed, hosted service may be offered commercially in future, for teams that
would rather not run StrataGraph themselves. If it happens, it sells **operation
and convenience, not the code**:

- The open client stays one hundred percent functional: no feature flags, no
  license checks. You can always self-host every capability.
- The paid value would be a service we operate (multi-tenant hosting, continuous
  indexing, an org-wide always-fresh graph, dashboards), not a richer binary.
- Because the engine is Apache-2.0 there is no resale restriction. The reason to
  pay would simply be that we run it for you, the model behind most successful
  open-source infrastructure.

This is a direction, not a product: there is no hosted service today. The
expression-of-interest form at [stratagraph.dev](https://stratagraph.dev/enterprise)
gauges whether teams want one.

## Trademark

"StrataGraph" and the logo are trademarks (see [../../TRADEMARK.md](../../TRADEMARK.md)).
Apache-2.0 covers the code, not the brand: forks are welcome but must not call
themselves StrataGraph. That is what would keep an official hosted offering
distinguishable from a fork.

## Contributions

Apache-2.0, under the DCO (see [../../CONTRIBUTING.md](../../CONTRIBUTING.md)).
