// repo-consumer has NO OpenAPI spec — it only CONSUMES the user service. The
// estate link pass must link these calls to the canonical getUser operation
// defined in repo-producer's spec (the cross-repo blast-radius case).

// Literal-URL consumer of GET /users/{id} → cross-repo CONSUMES getUser (0.70).
export async function fetchUserProfile(id: string) {
  const res = await fetch("/users/123");
  return res.json();
}

// operationId-name consumer (generated-client style) → CONSUMES getUser (0.75).
export async function getUserViaClient(id: string) {
  return getUser({ id });
}

// A call to an endpoint NO operation declares: must create NO CONSUMES edge.
export async function fetchWidget() {
  return fetch("/widgets/9");
}
