// The PRODUCERS of the GraphQL root fields. `getUser`/`createUser` are named
// functions (so PRODUCES attaches to their function nodes — the headline target
// is the `getUser` resolver here); `listUsers` is an inline arrow.
//
// repo-schema has NO consumers — it only declares + implements the schema.

export function getUser(_parent: any, args: { id: string }) {
  return { id: args.id, name: "Ada" };
}

export function createUser(_parent: any, args: { input: { name: string } }) {
  return { id: "1", name: args.input.name };
}

export const resolvers = {
  Query: {
    getUser,
    listUsers: () => [],
  },
  Mutation: {
    createUser,
  },
};
