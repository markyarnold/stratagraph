"""Shapes: each class has an `area` and a `describe` method. `area` and
`scale` are shared method names across classes (the unknown-receiver ambiguity);
`describe` calls `self.area()` (Inferred self-method)."""


class Rectangle:
    def __init__(self, w, h):
        self.w = w
        self.h = h

    def area(self):
        return self.w * self.h

    def scale(self, factor):
        # self.<method> on the enclosing class -> Inferred.
        self.resize(factor)
        return self

    def resize(self, factor):
        self.w = self.w * factor
        self.h = self.h * factor

    def describe(self):
        # self.area() -> Inferred (own-class method).
        return self.area()


class Circle:
    def __init__(self, r):
        self.r = r

    def area(self):
        return 3 * self.r * self.r

    def scale(self, factor):
        # self.<method> on the enclosing class -> Inferred.
        self.resize(factor)
        return self

    def resize(self, factor):
        self.r = self.r * factor

    def describe(self):
        # self.area() -> Inferred.
        return self.area()
