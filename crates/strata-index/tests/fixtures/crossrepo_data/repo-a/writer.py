# repo-a code that WRITES the tables via raw-SQL string literals (asyncpg-style
# `$1`/`$2` positional placeholders — native Postgres syntax that parses cleanly).
# The data plane parses each literal and adds a `Writes` edge from the enclosing
# function → the declared Table (Extracted 0.95) — the write half of the §6.2
# flagship, so `impact(users)` reaches `touch_last_login` and `impact(memberships)`
# reaches `add_membership`.


def touch_last_login(user_id):
    conn.execute("UPDATE users SET last_login = now() WHERE id = $1", user_id)


def add_membership(user_id, org_id):
    conn.execute(
        "INSERT INTO memberships (user_id, org_id) VALUES ($1, $2)",
        user_id,
        org_id,
    )


# A dynamic query (f-string) is NOT a single literal → honestly NOT linked. This
# proves the never-invent rule: no Writes edge is created for an interpolated query.
def delete_by_table(table, row_id):
    conn.execute(f"DELETE FROM {table} WHERE id = $1", row_id)
