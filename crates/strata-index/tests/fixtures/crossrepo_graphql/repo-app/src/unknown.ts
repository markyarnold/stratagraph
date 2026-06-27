// A gql query for a field NO schema declares: must produce NO CONSUMES edge —
// surfaced by absence, never invented into a link.

import { gql } from "@apollo/client";

export async function loadUnknown() {
  const query = gql`
    query {
      nonExistentField {
        x
      }
    }
  `;
  return query;
}
