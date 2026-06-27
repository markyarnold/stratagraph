import { foo as bar } from "./b";
import * as NS from "./ns";
import { fooReexport } from "./barrel";
export function run() {
  bar();
  NS.nsFn();
  fooReexport();
  dup();
}
function dup() {}
