export function getUser(_p: any, args: { id: string }) {
  return { id: args.id, name: "x" };
}

export const resolvers = {
  Query: {
    getUser,
  },
};
