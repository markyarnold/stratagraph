import express from "express";

const app = express();

// The PRODUCER of GET /users/{id}: the getUser handler. The estate link pass
// gives it a PRODUCES edge to the ONE canonical getUser operation.
export function getUser(req: any, res: any) {
  res.json({ id: req.params.id });
}
app.get("/users/:id", getUser);

export { app };
