"""Rendering: unknown-receiver `.area()` / `.scale()` fan-outs (Ambiguous, two
candidates each), import-matched constructor calls (Inferred), and a dynamic
getattr call (never guessed)."""

from .shapes import Rectangle, Circle


def make_rectangle(w, h):
    # Import-matched + unique repo-wide name -> Inferred.
    return Rectangle(w, h)


def make_circle(r):
    # Import-matched + unique repo-wide name -> Inferred.
    return Circle(r)


def total_area(shape_a, shape_b):
    # `.area()` on UNANNOTATED params: the heuristic fans out over both `area`
    # methods (Ambiguous), and scip-python — with no receiver type to go on —
    # also cannot resolve them (an honest SCIP gap: unadjudicable, surfaced).
    a = shape_a.area()
    b = shape_b.area()
    return a + b


def total_area_typed(rect: Rectangle, circ: Circle):
    # TYPE-ANNOTATED receivers: scip-python resolves `.area()` precisely
    # (Rectangle.area, Circle.area), while the heuristic still fans out over both
    # -> adjudicable Ambiguous (1 confirmed + 1 denied each).
    a = rect.area()
    b = circ.area()
    return a + b


def grow_all(shape_a, shape_b, factor):
    # `.scale()` on UNANNOTATED params -> Ambiguous for the heuristic, unadjudicable
    # for SCIP (no receiver type).
    shape_a.scale(factor)
    shape_b.scale(factor)


def grow_typed(rect: Rectangle, circ: Circle, factor):
    # TYPE-ANNOTATED receivers: scip-python resolves `.scale()` precisely while the
    # heuristic fans out over both `scale` methods -> adjudicable Ambiguous.
    rect.scale(factor)
    circ.scale(factor)


def dynamic_area(shape, method_name):
    # getattr(...)() is dropped by the extractor — never a guessed call edge.
    return getattr(shape, method_name)()
