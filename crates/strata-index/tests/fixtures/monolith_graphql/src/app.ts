// CONSUMERS in the same repo, two styles:
//  - `loadUser`: a `gql` TAGGED query reading Query.getUser → CONSUMES Extracted
//    (0.95) from the enclosing `loadUser` function node.
//  - `listAll`: an UNTAGGED template constant (the dominant AppSync/Amplify
//    style) reading Query.listUsers. Once parsed it is evidence-identical to a
//    tagged doc → CONSUMES Extracted (0.95) from `listAll`.
// A non-GraphQL template constant in the same file (`BUTTON_CSS`) must produce no
// edge and must not be counted as an unparsed document (it never claimed to be
// GraphQL).

import { gql } from "@apollo/client";

export async function loadUser(id: string) {
  const query = gql`
    query GetUser {
      getUser(id: "1") {
        name
      }
    }
  `;
  return query;
}

export async function listAll() {
  // Untagged template constant — the AppSync/Amplify style the tagged-only
  // extractor missed. Parse-gated, then linked exactly like a tagged doc.
  const LIST_USERS = `
    query ListUsers {
      listUsers {
        id
        name
      }
    }
  `;
  return LIST_USERS;
}

// A non-GraphQL template constant: the prefilter keeps it out of the pipeline, so
// no CONSUMES edge and no unparsed-document count.
const BUTTON_CSS = `color: red; padding: 0`;
