// The PRODUCER of the user service's Query.getUser.

export function getUser(_parent: any, args: { id: string }) {
  return { id: args.id, name: "Ada" };
}

export const resolvers = {
  Query: {
    getUser,
  },
};
