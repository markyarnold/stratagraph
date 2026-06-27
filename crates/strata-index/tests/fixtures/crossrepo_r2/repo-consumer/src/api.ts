// Consumer repo with NO spec. Even though repo-producer has a malformed spec
// alongside its valid one, this consumer's call to getUser must still link
// cross-repo to the canonical getUser operation (R2: a broken spec degrades
// gracefully; valid links are unaffected).

export async function loadUser(id: string) {
  const res = await fetch("/users/123");
  return res.json();
}
