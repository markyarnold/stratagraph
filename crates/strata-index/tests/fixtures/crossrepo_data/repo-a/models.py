# repo-a ORM models (Slice 25, D3, M2b). A SQLAlchemy model with an EXPLICIT
# `__tablename__` mapping the `User` class to the declared `users` table. The data
# plane adds a `User —MapsTo→ users` edge (Extracted 0.95), so `impact(users)`
# reaches the model and (transitively) any code that instantiates it. Only the
# explicit literal name is captured; a convention-derived name would be a future
# Inferred tier (deferred).


class User(Base):
    __tablename__ = "users"

    id = Column(Integer, primary_key=True)
    email = Column(String)
