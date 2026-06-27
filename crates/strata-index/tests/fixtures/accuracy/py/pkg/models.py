"""Domain models for the accuracy corpus."""


def make_user(name):
    return {"name": name}


class User:
    def __init__(self, name):
        self.name = name

    def save(self):
        # self.<method> on the enclosing class → Inferred (own-class method).
        self.validate()

    def validate(self):
        return True


class Account:
    def save(self):
        # A second `save` method repo-wide — makes an unknown-receiver `.save()`
        # call ambiguous (two candidates).
        return True
