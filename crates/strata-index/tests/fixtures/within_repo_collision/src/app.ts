// A consumer of `getUser` in a repo where TWO schemas declare that key. The
// linker cannot honestly know which API this document targets.
import { gql } from "@apollo/client";

export async function loadUser(id: string) {
  const query = gql`
    query GetUser {
      getUser(id: "1") {
        id
      }
    }
  `;
  return query;
}
