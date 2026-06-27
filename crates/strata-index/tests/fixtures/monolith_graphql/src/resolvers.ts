// Apollo-style resolver map: the PRODUCERS of the GraphQL root fields.
// `getUser`/`createUser` are named functions (so the PRODUCES edge attaches to
// their function nodes); `listUsers` is an inline arrow (attaches to the module).

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
