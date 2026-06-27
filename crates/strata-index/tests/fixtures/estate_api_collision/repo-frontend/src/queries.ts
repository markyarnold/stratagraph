// A consumer with NO schema of its own. Its `getUser` query matches the
// canonical key `Query.getUser` — which is declared by BOTH the user service and
// the (unrelated) billing service. With no api id declared for either, the
// honest answer is an Ambiguous fan-out to both candidates (0.35 each), never a
// silent confident pick of one. This is the regression: pre-fix, this consumer
// was confidently linked to a single merged node, dragging an unrelated repo
// into the blast radius at 0.76, unflagged.

import { gql } from "@apollo/client";

export async function loadUser() {
  const query = gql`
    query GetUser {
      getUser(id: "1") {
        id
      }
    }
  `;
  return query;
}
