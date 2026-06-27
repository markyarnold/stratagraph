import express from "express";

const app = express();

// Single match: GET /users/{id} (openapi.yaml) -> Inferred PRODUCES.
export function getUser(req: any, res: any) {
  res.json({ id: req.params.id });
}
app.get("/users/:id", getUser);

// Ambiguous: GET /things/{id} is declared in BOTH things-v1.yaml and
// things-v2.yaml -> two Ambiguous PRODUCES edges.
export function getThing(req: any, res: any) {
  res.json({ id: req.params.id });
}
app.get("/things/:id", getThing);

// No matching operation: no PRODUCES edge (route implements something the
// specs don't declare).
export function removeIt(req: any, res: any) {
  res.status(204).end();
}
app.delete("/nonexistent", removeIt);

export { app };
