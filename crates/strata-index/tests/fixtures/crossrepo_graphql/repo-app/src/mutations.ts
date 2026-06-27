// UNTAGGED template constants — the dominant AppSync/Amplify consumer style the
// tagged-only extractor missed entirely. repo-app declares NO schema, so these
// link cross-repo to the canonical fields in repo-schema.
//
//  - `CREATE_USER`: a substitution-free template constant reading
//    Mutation.createUser. Once parsed it is evidence-identical to a tagged doc →
//    cross-repo CONSUMES Extracted 0.95.
//  - `BROKEN_UNTAGGED`: passes the cheap prefilter (starts with `{`) but is NOT
//    valid GraphQL. It must produce NO link AND must NOT be counted as an
//    unparsed document — an untagged candidate never claimed to be GraphQL, so a
//    parse failure is silently skipped (the honesty rule), unlike a tagged miss.

export const CREATE_USER = `
  mutation CreateUser {
    createUser(input: { name: "Ada" }) {
      id
      name
    }
  }
`;

// Prefilter says "maybe GraphQL" (leading `{`), parser says "no". Silently
// dropped: no edge, not counted unparsed.
export const BROKEN_UNTAGGED = `{ this is not valid graphql `;
