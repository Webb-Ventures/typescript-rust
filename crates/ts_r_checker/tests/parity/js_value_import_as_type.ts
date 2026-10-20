// @filename: tsconfig.json
{"compilerOptions": {"allowJs": true, "checkJs": true, "noEmit": true, "strict": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "Bundler"}, "files": ["values.js", "merged.ts", "eq.ts", "a.js"]}
// @filename: values.js
export function make() { return 1; }
export const count = 2;
export class Thing {}
// @filename: merged.ts
interface Both { x: number }
const Both = 1;
export default Both;
// @filename: eq.ts
export = class Eq {};
// @filename: a.js
// A JSDoc type naming an imported value is TS2749, unless the import also has a type side.
import { make, count, Thing } from "./values.js";
import Both from "./merged";
import Eq from "./eq";
/** @type {make} */
let a;
/** @type {count} */
let b;
/** @type {Thing} */
let c;
/** @type {Both} */
let d;
/** @type {Eq} */
let e;
export { a, b, c, d, e };
