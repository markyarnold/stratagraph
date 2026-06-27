import express from "express";

const app = express();

// The PRODUCER of service A's GET /health.
export function health(req: any, res: any) {
  res.json({ ok: true, service: "a" });
}
app.get("/health", health);

export { app };
