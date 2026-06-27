import requests
import graphene
from gql import gql


# Producer: a FastAPI/Flask-2 route decorator. Links GET /users/{id} -> get_user.
@app.get("/users/{id}")
def get_user(id):
    return {"id": id}


# REST consumer: a requests call. Links fetch_widget -> GET /widgets/{id}.
def fetch_widget(wid):
    return requests.get("/widgets/1")


# GraphQL consumer: a gql() document. Links load_user -> Query.getUser.
def load_user(client):
    return client.execute(gql("query { getUser { id } }"))


# GraphQL producer: a Graphene resolver host. Links resolve_getUser -> Query.getUser.
class Query(graphene.ObjectType):
    user = graphene.Field("User")

    def resolve_getUser(self, info):
        return None


# Django views (same-file, so the producer attributes to the function node).
def health_check(request):
    return None


def thing_detail(request, pk):
    return None


# Django URLconf: method-less producer routes (the view dispatches HTTP methods).
# "health/" matches exactly one operation  -> path-only Inferred 0.65;
# "things/<int:pk>/" matches two operations at the same path -> Ambiguous 0.35 each.
from django.urls import path

urlpatterns = [
    path("health/", health_check),
    path("things/<int:pk>/", thing_detail),
]
