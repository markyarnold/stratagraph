// repo-a ORM model (Slice 25, D3, M2b). A TypeORM entity with an EXPLICIT
// `@Entity("orgs")` decorator mapping the `Org` class to the declared `orgs` table.
// The data plane adds an `Org —MapsTo→ orgs` edge (Extracted 0.95). The decorator is
// hoisted to the `export` statement by the grammar — the extractor handles that. Only
// the explicit literal name is captured (convention deferred).

@Entity("orgs")
export class Org {
  id: number;
  name: string;
}
