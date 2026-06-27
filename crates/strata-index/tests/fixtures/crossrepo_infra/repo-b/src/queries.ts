// repo-b has NO schema and NO infrastructure — it is a pure consumer. Its `gql`
// query reads Query.getUser, declared (and PRODUCED via AppSync) in repo-a. The
// estate link must carry the cross-repo CONSUMES so impact(Query.getUser) reaches
// this consumer in the OTHER repo.

import { gql } from "graphql-tag";

export function loadUser() {
  return gql`
    query GetUser {
      getUser(id: "1") {
        id
        name
      }
    }
  `;
}
