// repo-a code that READS the `users` table via a raw-SQL string literal. The data
// plane parses the literal, matches `users` to the declared Table, and adds a
// `Reads` edge from `getUserEmail` → users (Extracted 0.95). This is the code half
// of the §6.2 flagship: `impact(users)` (or impact(users.email)) reaches this fn.
import { db } from "./db";

export function getUserEmail(id: number): Promise<string> {
  return db.query("SELECT email FROM users WHERE id = $1", [id]);
}

// A JOIN read: reaches BOTH users and orgs (two Reads edges).
export function listUsersWithOrg(): Promise<unknown[]> {
  return db.query(
    "SELECT u.email, o.name FROM users u JOIN orgs o ON o.id = u.org_id",
  );
}
