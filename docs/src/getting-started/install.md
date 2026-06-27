# Install

StrataGraph is a single Rust binary, `strata`. You build it from source, put it on
your `PATH`, and verify it. This page takes you through that, end to end, and is
honest about which platforms are exercised today.

## Prerequisites

- **A Rust toolchain** (stable). Install it from [rustup.rs](https://rustup.rs);
  the build uses whatever stable `cargo`/`rustc` `rustup` gives you. There is no
  pinned toolchain file and no exotic MSRV; a recent stable toolchain builds the
  workspace.
- **Git.** The build also reads the current git commit to stamp an *engine id*
  into the binary (see [Verify the install](#verify-the-install)). Building from
  a git checkout is the normal path; a build with no git still works, it just
  reports the engine id as `unknown`.
- A C toolchain is pulled in transitively by the tree-sitter grammars; on macOS
  the Xcode command-line tools cover it, on Linux a standard `build-essential`
  equivalent does.

## Build from source

From the repository root:

```bash
cargo build --release
```

This compiles the whole workspace. The `strata` binary lands at:

```text
target/release/strata
```

A `--release` build is what you want for day-to-day use; it is dramatically
faster at indexing than a debug build. (A debug build, `cargo build`, lands at
`target/debug/strata` and is useful while hacking on StrataGraph itself.)

## Put `strata` on your PATH

The binary is self-contained, so you only need to make it reachable. Two common
patterns:

**Symlink into a directory already on your PATH** (recommended, since a rebuild is
picked up automatically, no copying):

```bash
ln -sf "$(pwd)/target/release/strata" /usr/local/bin/strata
```

**Or add the target directory to your PATH** in your shell profile:

```bash
export PATH="$HOME/code/strata/target/release:$PATH"
```

The symlink pattern is the one to prefer: every time you `cargo build --release`,
the symlink resolves to the fresh binary, so `strata` on your PATH is never a
stale copy.

## Verify the install

```bash
strata --version
```

You should see the package version followed by the **engine id** in parentheses:

```text
strata 0.1.0 (405a1ba2dedd-dirty)
```

The engine id is the short git commit the binary was built from, with a `-dirty`
suffix when the working tree had uncommitted changes at build time. It is printed
here, in every index summary, and in the desktop footer for one reason: so a
stale binary on your PATH is identifiable at a glance. If two surfaces disagree
about what depends on a symbol, the first thing to check is whether their engine
ids match. (A build made outside a git checkout reports `unknown` rather than
guessing.)

Confirm the subcommands are present:

```bash
strata --help
```

```text
Commands:
  index           Build or refresh the code graph for a repository (or a workspace estate)
  impact          Show the reverse blast radius (dependents) of a symbol
  explain         Explain WHY one symbol is in another's blast radius: the evidence chain ...
  context         Show the 360° context of a symbol (callers, callees, imports, members)
  query           Lexical search over node name, fully-qualified name, and path
  mcp             Serve the code graph to an MCP client over stdio
  detect-changes  Report the changed symbols, blast radius, and risk vs HEAD ...
  blast           Report the pre-edit blast radius of a FILE ...
  rename          Graph-aware multi-file rename of a code symbol ...
  init            Install a strictly-governed agent-integration kit ...
```

## Platform status

Be aware of what is actually exercised:

- **macOS** is the primary, daily-driven platform. If you are on macOS, expect
  the smoothest experience.
- **Linux** builds from the same source with the same toolchain. The core engine
  is portable Rust with no macOS-specific dependencies, so it is expected to work,
  but it is less heavily exercised than macOS; treat it as supported-but-test-it.
- **Windows** is not a tested target today. The engine has no intentional
  platform lock-in, but paths, the shell-based agent-kit hooks, and the build have
  not been validated there. If you need Windows, WSL2 (a Linux environment) is the
  pragmatic route.

## Optional extras

The CLI above is all you need to index a repo, run queries, and serve the MCP
server. Two further pieces are optional:

- **The desktop app** (a Tauri application) gives you a graph view and
  point-and-click query / context / impact panels. It is a separate build with a
  Node/Tauri toolchain of its own. It is pre-release; see
  [The desktop app](desktop.md).
- **This documentation** is an [mdBook](https://rust-lang.github.io/mdBook/). If
  you have `mdbook` installed you can build it from the `docs/` directory with
  `mdbook build` (or `mdbook serve` for a live preview); it is not required to use
  StrataGraph.

## Next

Index your first repository: [Index your first repository](first-index.md).
