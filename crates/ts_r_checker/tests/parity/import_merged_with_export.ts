// @filename: m.ts
export default { foo: 12 };
export function f() {}
export const v = 1;
// @filename: a.ts
// An import merged with an exported local declaration mixes local and exported parts.
import Obj from "./m";
export const Obj = 1;
// @filename: b.ts
import { f } from "./m";
export function f() {}
// @filename: c.ts
import { v } from "./m";
export interface v {}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "esnext", "noEmit": true }, "files": ["m.ts", "a.ts", "b.ts", "c.ts"] }
