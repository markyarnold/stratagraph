// repo-b has NO database schema — a pure code repo. It proves the data plane is
// silent when no `.sql` schema is present (it contributes zero tables/columns).
export function greet(name: string): string {
  return `hello ${name}`;
}
