// @filename: a.ts
export default class A {}
export class B {}
export class C {}
// @filename: b.ts
// A type-only import with both a default and named bindings is TS1363.
import type A, { B, C } from "./a";
import type D, * as ns from "./a";
// @filename: c.ts
namespace n {
  export class Foo {}
}
import type Foo = n.Foo;
// @filename: d.ts
import type * as a from "./a";
import B = a.B;
import aa = a;
export type { C } from "./a";
export { a, B };
// @filename: e.ts
import * as d from "./d";
import B = d.a.B;
import BB = d.B;
import C = d.C;
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "commonjs", "noEmit": true }, "files": ["a.ts", "b.ts", "c.ts", "d.ts", "e.ts"] }
