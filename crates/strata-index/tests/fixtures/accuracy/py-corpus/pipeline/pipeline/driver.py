"""The pipeline driver: cross-module import-matched calls (Inferred), a
unique-repo-wide bare name without an import (Inferred), and an unknown bare name
that resolves to nothing (unresolved, never invented)."""

from .stages import parse, normalize


def run(raw):
    # Import-matched cross-module calls -> Inferred.
    tokens = parse(raw)
    clean = normalize(tokens)
    # `summarize` is defined once repo-wide (below) and is NOT imported here, so
    # it resolves via the unique-repo-wide-name rule -> Inferred.
    return summarize(clean)


def summarize(tokens):
    return {"count": len(tokens)}


def broken(raw):
    # `missing_stage` is defined NOWHERE -> no edge (unresolved, surfaced).
    return missing_stage(raw)
