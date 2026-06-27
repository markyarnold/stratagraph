"""A service module linking across the package: import-matched cross-module
calls (Inferred), same-module calls (Extracted), an unknown-receiver method
fan-out (Ambiguous), and a dynamic getattr call that must NEVER be guessed."""

from .models import Cart, Invoice, sum_prices


def price_cart(lines):
    # Import-matched cross-module call: `sum_prices` is bound by the import to
    # shop/models.py which defines it -> Inferred.
    return sum_prices(lines)


def checkout(lines):
    # Same-module bare call -> Extracted.
    cart = build_cart(lines)
    # Another same-module bare call -> Extracted.
    inv = build_invoice(lines)
    # Unknown-receiver `.total()` with TWO `total` methods repo-wide (Cart,
    # Invoice) -> Ambiguous fan-out. The receiver's concrete type is unknown to
    # the heuristic; SCIP resolves it to Cart.total.
    cart_total = cart.total()
    # A second unknown-receiver `.total()`; SCIP resolves this one to
    # Invoice.total. Same Ambiguous fan-out for the heuristic.
    inv_total = inv.total()
    return cart_total + inv_total


def build_cart(lines):
    # Import-matched: `Cart` is imported from shop/models.py. (A class call, not
    # a function, but the import binding + unique repo-wide name still resolve.)
    return Cart(lines)


def build_invoice(lines):
    return Invoice(sum(lines))


def reconcile(cart: Cart, inv: Invoice):
    # TYPE-ANNOTATED receivers: scip-python resolves `.total()` precisely by the
    # annotated type, while the heuristic (no type system) still fans out over
    # BOTH `total` methods repo-wide -> Ambiguous. These are the adjudicable
    # AMBIGUOUS sites the calibration grades: SCIP confirms Cart.total here and
    # Invoice.total below; the heuristic's extra candidate is a denial each time.
    a = cart.total()
    b = inv.total()
    # `.tax()` is unique to Invoice repo-wide, but an unknown-receiver method call
    # is ALWAYS Ambiguous (no receiver type) — a single-candidate Ambiguous that
    # SCIP confirms exactly (1 confirmed, 0 denied).
    t = inv.tax()
    # `.subtotal()` is unique to Cart; same single-candidate Ambiguous, confirmed.
    s = cart.subtotal()
    return a + b + t + s


def render(report, name):
    # Dynamic dispatch is NEVER linked: a getattr(...)() callee is dropped by the
    # extractor, so this site produces no heuristic edge and SCIP (any-typed
    # report) does not adjudicate it either.
    return getattr(report, name)()
