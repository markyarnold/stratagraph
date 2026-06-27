"""A Python module in the mixed-language fixture: a class whose method calls a
same-module function, plus a module-level function. Linked entirely within the
Python resolution world (no cross-language edge to the TS plane)."""


def py_helper():
    return 7


class Service:
    def run(self):
        # Bare call to a same-module function → Extracted 0.95.
        return py_helper()
