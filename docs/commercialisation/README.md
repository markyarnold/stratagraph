# StrataGraph: license and the (possible) hosted option

StrataGraph is **source available under the Functional Source License v1.1
(FSL-1.1-ALv2)** ([../../LICENSE.md](../../LICENSE.md)). The entire suite is in this
repository and free to read, run, modify, self-host and redistribute for any purpose
other than competing with us: the `strata` CLI and all its tools, the MCP server, the
desktop app, the agent kit, and multi-repo estates. The org-scale capabilities still
on the roadmap (hosted estates at scale, history and trends, team collaboration, PR/CI
governance, SSO/RBAC/audit) will land here too, under the same terms. Nothing is
paywalled, and **each release becomes Apache 2.0 two years after it ships**.

## Why source available, not fully open

The FSL is the lightest restriction that protects one thing: nobody may take the engine
and sell it as a competing product or service while we build the business around it.
Everything else, internal use, self-hosting, modification, redistribution, education
and research, is free. After two years each release converts to the Apache License 2.0,
so the restriction is time-limited by design and the project becomes fully open on a
rolling basis. It keeps the source readable and auditable (you can inspect every edge)
while keeping a future hosted offering viable.

## The (possible) commercial layer

A managed, hosted service may be offered commercially in future, for teams that
would rather not run StrataGraph themselves. If it happens, it sells **operation
and convenience, not access to the code**:

- The source client stays one hundred percent functional: no feature flags, no
  license checks. You can always self-host every capability.
- The paid value would be a service we operate (multi-tenant hosting, continuous
  indexing, an org-wide always-fresh graph, dashboards), not a richer binary.
- The one thing the FSL does not permit is reselling the engine as a competing
  product or service. That is what keeps an official hosted offering viable while the
  source stays open to read and free for every non-competing use. The reason to pay
  would simply be that we run it for you, the model behind most successful
  source-available infrastructure.

This is a direction, not a product: there is no hosted service today. The
expression-of-interest form at [stratagraph.dev](https://stratagraph.dev/enterprise)
gauges whether teams want one.

## Trademark

"StrataGraph" and the logo are trademarks (see [../../TRADEMARK.md](../../TRADEMARK.md)).
The FSL covers the code, not the brand: forks are welcome but must not call
themselves StrataGraph. That is what would keep an official hosted offering
distinguishable from a fork.

## Contributions

Under the FSL, via the DCO (see [../../CONTRIBUTING.md](../../CONTRIBUTING.md)).
