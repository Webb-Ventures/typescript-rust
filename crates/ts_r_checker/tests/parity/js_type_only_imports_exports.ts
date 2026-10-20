// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "ESNext", "moduleResolution": "bundler"}, "files": ["types.ts", "b.js"]}
// @filename: types.ts
// Checked JavaScript: importing a name that is only a type is TS18042 (at the imported
// name), exporting one (an interface, a JSDoc typedef) is TS18043.
export interface Shape { kind: string }
export type Kind = "a" | "b";
export class Box { v = 1 }
export interface Box { w: number }
export const value = 1;
namespace OnlyTypes { export type T = number; }
export default OnlyTypes;
// @filename: b.js
import Def, { Shape, Kind as K, Box, value } from "./types";

/** @typedef {{ x: number }} Local */
const v = value;

export { Local };
export { Local as Renamed };
export { v };
export { Box as B2 };
export { Kind } from "./types";

/** @type {Shape | K | Def.T | undefined} */
export let u;
console.log(Box, v);
