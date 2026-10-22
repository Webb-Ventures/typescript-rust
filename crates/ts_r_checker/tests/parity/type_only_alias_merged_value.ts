// An alias merged with a local value (`import { A }` of a type and `const A = 0`) exports
// that value: importers read it, and a namespace import has it as a property.
// @filename: z.ts
interface A {}
export type { A };
// @filename: b.ts
import { A } from "./z";
const A = 0;
export { A };
export class B {}
// @filename: c.ts
import { A } from "./b";
import * as types from "./b";
const n: number = A;
let t: typeof types = {
  A: undefined as any,
  B: undefined as any,
};
const s: string = types.A;
