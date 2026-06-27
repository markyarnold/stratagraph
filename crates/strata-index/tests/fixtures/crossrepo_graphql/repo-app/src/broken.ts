// An INTERPOLATED gql template: fragment composition makes the document text
// unreliable. It is recorded interpolation_free=false → counted as an unparsed
// document in coverage, NEVER linked (never confident-wrong).

import { gql } from "@apollo/client";

const userFields = gql`
  fragment UserFields on User {
    name
  }
`;

export async function loadComposed() {
  const query = gql`
    query {
      ${userFields}
      getUser(id: "1") {
        ...UserFields
      }
    }
  `;
  return query;
}
