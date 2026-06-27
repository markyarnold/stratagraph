// An uptime checker with NO spec. Its literal `fetch("/health")` matches the
// canonical key `GET /health` (operationId `health`), declared by BOTH service A
// and service B. With no api id declared, the honest result is an Ambiguous
// fan-out to both health operations (0.35 each) — never a confident pick of one
// unrelated service.

export async function probeHealth() {
  const res = await fetch("/health");
  return res.json();
}
