// The PRODUCER of the billing service's (unrelated) Query.getUser.

export function getUser(_parent: any, args: { id: string }) {
  return { id: args.id, balanceCents: 0 };
}

export const resolvers = {
  Query: {
    getUser,
  },
};
