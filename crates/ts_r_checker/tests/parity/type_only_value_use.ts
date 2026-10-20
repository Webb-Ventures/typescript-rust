// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "commonjs", "noEmit": true}, "files": ["a.ts", "b.ts", "c.ts", "d.ts", "e.ts"]}
// @filename: a.ts
export class A { a!: string }
export type { A as B };
export const onInit = Symbol("onInit");
// @filename: b.ts
import type { A } from "./a";
new A();
let a: A = { a: "" };
let b = { A };
type Q = typeof A;
declare class X extends A {}
export default A;
// @filename: c.ts
import type { A } from "./a";
export { A as D };
// @filename: d.ts
import { D } from "./c";
import { B } from "./a";
new D();
new B();
// @filename: e.ts
import type { onInit } from "./a";
interface Component {
    [onInit]?(): void;
}
const o = { [onInit]: 0 };
class C { [onInit] = 0; }
abstract class F { abstract [onInit](): void; }
export { o, C, F };
