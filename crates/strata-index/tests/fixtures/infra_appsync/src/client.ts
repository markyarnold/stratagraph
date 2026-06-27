// A frontend consumer of the GraphQL API. The UNTAGGED template constant reads
// Query.getUser; once parsed it is evidence-identical to a tagged document, so it
// CONSUMES Query.getUser at Extracted 0.95. This is the frontend half of THE
// PROOF: impact(Query.getUser) must reach BOTH the implementing Lambda (via the
// infra PRODUCES) and this consumer (via the contract CONSUMES).

const GET_USER = `query GetUser { getUser(id: "1") { id } }`;

export function loadUser() {
  return fetch("/graphql", {
    method: "POST",
    body: JSON.stringify({ query: GET_USER }),
  });
}

// A second consumer, reading Query.listUsers — the field PyFunction produces. This
// is the frontend half of the MODULE PROOF: impact(functions/py-op/app.py) reaches
// the Python Lambda (via Runs), the listUsers field it produces, and this module.
const LIST_USERS = `query ListUsers { listUsers { id } }`;

export function loadUsers() {
  return fetch("/graphql", {
    method: "POST",
    body: JSON.stringify({ query: LIST_USERS }),
  });
}
