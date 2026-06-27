// A TypeScript module in the mixed-language fixture. It defines and calls a
// function entirely within the TS resolution world — proving the TS plane is
// unchanged when a Python plane is present in the same repo.

export function tsHelper(): number {
  return 42;
}

export function tsMain(): number {
  return tsHelper();
}
