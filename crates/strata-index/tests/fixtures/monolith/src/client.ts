// A consumer in the SAME repo as the spec (the monolith case): both a literal
// URL fetch and a generated-client-style operationId-name call hit getUser.

export async function loadUser(id: string) {
  // Literal URL → Inferred 0.70 CONSUMES to getUser (GET /users/{id}).
  const res = await fetch("/users/123");
  return res.json();
}

export async function loadUserByName(id: string) {
  // operationId-name call → Inferred 0.75 CONSUMES to getUser.
  return getUser({ id });
}

// A call to an endpoint no operation declares: NO consumes edge (surfaced as
// unmatched, never invented).
export async function loadWidget() {
  return fetch("/widgets/9");
}
