"""Pipeline stages: module-level functions, some with repo-unique names reached
across modules (Inferred), some same-module (Extracted)."""


def parse(raw):
    # Same-module bare call -> Extracted.
    return tokenize(raw)


def tokenize(raw):
    return raw.split(",")


def normalize(tokens):
    # Same-module bare call -> Extracted.
    return dedupe(tokens)


def dedupe(tokens):
    out = []
    for t in tokens:
        if t not in out:
            out.append(t)
    return out
