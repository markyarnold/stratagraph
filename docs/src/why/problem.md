# The problem

Modern software fails at its seams. A single feature touches a frontend, a backend service, an API contract between them, a database, and the cloud infrastructure that runs and permits it all, often spread across several repositories. The damage from a change rarely stays in the file you edited. It propagates across the seams: an API producer changes shape and a consumer in another repository breaks; a database column is renamed and three services that query it start failing; an IAM role is tightened and a Lambda silently loses a permission it needs at runtime.

The tools meant to help you see this coming have two structural gaps, and a third problem has arrived on top of them.

## Problem 1: impact tools guess, and present guesses as fact

Most code-intelligence tools resolve references heuristically: match a called name against the names in scope and emit an edge. That works often enough to look right and fails quietly when it matters: dynamic dispatch, reflection, re-exports, method calls on a value whose type the tool never resolved. The honest answer in those cases is "I am not sure which target this is." The answer these tools give instead is a single confident edge, or a silently dropped one.

Both failure modes are corrosive in the same way: **you cannot tell a reliable result from an unreliable one.** When the tool says "these five functions depend on this," you do not know whether that is five facts, four facts and a guess, or four facts with a sixth dependency missing because the tool could not follow a dynamic call. A blast radius you cannot calibrate is a blast radius you cannot gate a deployment on. It is decoration, not evidence.

The consequence is learned distrust. After a tool confidently misses a breaking change once, you stop believing its "nothing depends on this", and then you are back to reading the code by hand, which is the work the tool was supposed to save.

StrataGraph's measured accuracy work exists precisely to refuse this trade-off; see [Confidence and provenance](../concepts/confidence.md) and the [accuracy reports](../accuracy/results.md).

## Problem 2: impact stops at language and repository boundaries

A call-graph tool sees calls. It does not see the boundaries where most real breakage happens, because those boundaries are not function calls:

- **A GraphQL field or REST operation** is a contract between a producer and a consumer that may live in different repositories, written in different languages. To a call-graph tool, the resolver that implements `Query.getPolicyStats` and the frontend that queries it are two unrelated islands. Change the field and the tool sees nothing cross that gap.
- **A database table or column** is read and written by code through ORMs and query strings. Rename a column and the call graph is silent: the dependency runs through SQL and schema, not through a function it can trace.
- **An IAM role, a Lambda, an EventBridge rule** are infrastructure defined in YAML or HCL, not in your language at all. A call-graph tool does not parse them, so the relationship "this role is assumed by this Lambda, which runs this handler, which serves this API operation" is entirely invisible.
- **A cross-repository API call** crosses the one boundary single-repo tools are built to stop at. Index repo A and repo B separately and nothing tells you that a change in A breaks B.

So even a *perfectly accurate* call graph answers a question that is too small. It tells you what breaks **inside this language, inside this repository**, and stays silent about the contract, data, and infrastructure seams where cross-team, cross-service, production-grade breakage actually originates.

### A concrete scenario

You maintain a backend service. You want to remove a field from a GraphQL schema that "looks unused": grep finds no resolver in this repo that references it by name, so you delete it.

What grep and a call-graph tool could not see:

- a **Lambda resolver** in the infrastructure repo implements that field (the link runs through the SAM template's `CodeUri`, not through a call);
- a **frontend in a separate repository** queries the field in a GraphQL document;
- the field's identity is shared across the estate through the contract plane, so "who consumes this" has an answer; it just is not in the file you are looking at.

The deploy goes out. The frontend breaks. Nothing in your local tooling warned you, because the dependency never took the form of a function call in your repo. StrataGraph is built to answer exactly this; see [Cross-boundary impact](../concepts/cross-boundary.md) and the [dead-surface guide](../guides/dead-surface.md), which also shows the inverse: a field that genuinely *is* dead (no producer **and** no consumer) and is safe to delete.

## Problem 3: AI coding agents edit blind to blast radius

AI coding agents have made both problems sharper and more frequent. An agent edits fast, across many files, often in code it has never seen, and it edits with the same blind spot a human has with grep, only at higher velocity. It reads the file in front of it, makes a locally sensible change, and has no structural knowledge of what depends on the symbol it just altered. The contract a frontend relies on, the column a sibling service reads, the permission a Lambda needs: none of it is in the agent's context window, so none of it informs the edit.

A grep-based agent will also confidently tell you "nothing depends on this" when it means "I found no textual matches in the files I searched", which is not the same statement, and is wrong exactly when the dependency crosses a boundary grep cannot see.

The result is a fast loop that produces broad, plausible, and occasionally boundary-breaking changes, with no automatic check standing between the edit and the seam it might cross. What an agent needs is a structural, cross-boundary blast-radius check it can consult *before* it commits to a change, that is honest about its own uncertainty. That is what StrataGraph provides through its [MCP server](../reference/mcp.md) and the [agent kit](../getting-started/agent-kit.md).

---

These three problems share one root: the absence of a single, trustworthy, cross-boundary model of how a system actually fits together. The next page, [the StrataGraph approach](approach.md), describes how StrataGraph builds one.
