// @filename: m.ts
export default { foo: 12 };
// @filename: lib.d.ts
declare const _: _.Static;
declare namespace _ {
    interface Static {
        pick(): void;
    }
}
export = _;
// @filename: pick.d.ts
import { pick } from "./lib";
export = pick;
// @filename: a.ts
// A value read of an import merged with an exported variable is that variable.
import Obj from "./m";
export const Obj = void Obj;
// @filename: b.ts
// A namespace import of a module whose `export =` is a method of an object's type is a
// type: no TS2395 next to an exported variable.
import * as pick from "./pick";
export const pick = 1;
// @filename: c.ts
// `void x`'s operand is checked later: not a circular initializer (no TS7022).
export const x = void x;
const y = typeof y;
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "commonjs", "noEmit": true }, "files": ["m.ts", "lib.d.ts", "pick.d.ts", "a.ts", "b.ts", "c.ts"] }
