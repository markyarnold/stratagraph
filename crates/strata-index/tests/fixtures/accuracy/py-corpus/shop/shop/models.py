"""Domain models. Two classes deliberately share a `total` method name so an
unknown-receiver `.total()` call fans out AMBIGUOUSLY; SCIP narrows it by type."""


class Cart:
    def __init__(self, lines):
        self.lines = lines

    def total(self):
        # self.<method> on the enclosing class -> Inferred (own-class method).
        return self.subtotal()

    def subtotal(self):
        # A same-module bare call to a module-level helper -> Extracted.
        return sum_prices(self.lines)


class Invoice:
    def __init__(self, amount):
        self.amount = amount

    def total(self):
        # A second `total` method repo-wide: makes `.total()` on an unknown
        # receiver ambiguous (two candidates), and `self.tax()` Inferred.
        return self.amount + self.tax()

    def tax(self):
        return self.amount * 0.2


def sum_prices(lines):
    # Same-module bare call -> Extracted.
    return fold_prices(lines)


def fold_prices(lines):
    total = 0
    for line in lines:
        total = total + line
    return total
