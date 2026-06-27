// The Lambda handler the `UserFunction` Handler `user.handler` resolves to. Its
// presence (one indexed module at src/handlers/user.ts) is what lets the
// `Runs` bridge resolve to this module at Extracted 0.95.

export function handler() {
  return { statusCode: 200, body: "ok" };
}
