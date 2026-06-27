import express from "express";

const app = express();

// The PRODUCER of service B's GET /health — an entirely unrelated service that
// happens to expose the same path + operationId. Must never merge with svc-a.
export function health(req: any, res: any) {
  res.json({ ok: true, service: "b" });
}
app.get("/health", health);

export { app };
