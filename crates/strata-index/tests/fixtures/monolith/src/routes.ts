import express from "express";

const app = express();

// Producer: GET /users/{id} -> Inferred PRODUCES to the getUser operation.
export function getUser(req: any, res: any) {
  res.json({ id: req.params.id });
}
app.get("/users/:id", getUser);

export { app };
