export function getUser(_p: any, args: { id: string }) {
  return { id: args.id, name: "y" };
}

export const resolvers = {
  Query: {
    getUser,
  },
};
