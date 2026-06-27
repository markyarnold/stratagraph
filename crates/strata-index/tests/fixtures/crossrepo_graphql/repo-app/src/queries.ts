// repo-app has NO schema — it only CONSUMES the user service. The estate link
// pass must link this gql query to the canonical Query.getUser declared in
// repo-schema (the cross-repo GraphQL blast-radius case).

import { gql } from "@apollo/client";

// A gql query consuming Query.getUser → cross-repo CONSUMES (Extracted 0.95).
export async function loadUserProfile() {
  const query = gql`
    query GetUser {
      getUser(id: "1") {
        name
      }
    }
  `;
  return query;
}
