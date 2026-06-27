"""The Python Lambda handler the `PyFunction` resolource resolves to.

`PyFunction`'s `Handler: app.handler` + `CodeUri: functions/py-op/` resolves to
this file (`functions/py-op/app.py`). With the Python code plane (Slice 9) this
module is indexed and gets a `Module` node, so the infra `Runs` bridge now lands
on it at Extracted 0.95 — the EARNED flip (this file used to have no plane, so
`PyFunction` was pinned as `handler unresolved`)."""


def _build_response():
    return {"statusCode": 200, "body": "ok"}


def handler(event, context):
    # A same-module call so the handler is a real, connected code node.
    return _build_response()
