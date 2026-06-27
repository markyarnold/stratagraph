"""A service module that links across the package."""

from .models import make_user


def build_user(name):
    # Import-matched cross-module call → Inferred.
    return make_user(name)


def helper():
    return 1


def run():
    # Same-module bare call → Extracted.
    helper()
    # Unknown-receiver method call `acct.save()` — two `save` methods repo-wide
    # → Ambiguous fan-out.
    acct = object()
    acct.save()
    # Dynamic dispatch is never linked.
    getattr(acct, "nope")()
