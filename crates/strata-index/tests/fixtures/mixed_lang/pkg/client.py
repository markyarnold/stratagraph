"""A second Python module that imports from a sibling and calls across modules —
exercising relative-import resolution within the Python plane."""

from .service import py_helper


def use_helper():
    # Import-matched cross-module call → Inferred 0.80.
    return py_helper()
