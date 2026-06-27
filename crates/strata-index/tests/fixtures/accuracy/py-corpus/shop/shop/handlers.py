"""Request handlers: a cross-module import chain (Inferred), more same-module
calls (Extracted), and a unique-repo-wide bare name (Inferred)."""

from .service import price_cart, checkout


def handle_price(request):
    # Import-matched cross-module call -> Inferred.
    return price_cart(request)


def handle_checkout(request):
    # Import-matched cross-module call -> Inferred.
    result = checkout(request)
    # Same-module bare call -> Extracted.
    return wrap(result)


def wrap(value):
    # A repo-unique function name (`envelope` exists exactly once repo-wide, in
    # this file) reached via a same-module bare call -> Extracted.
    return envelope(value)


def envelope(value):
    return {"ok": True, "value": value}
