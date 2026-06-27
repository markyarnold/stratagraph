// The Lambda handler `UserFunction`'s `user.handler` resolves to, so the `Runs`
// bridge links this Lambda to this module.

export function handler() {
  return { statusCode: 200, body: "ok" };
}
