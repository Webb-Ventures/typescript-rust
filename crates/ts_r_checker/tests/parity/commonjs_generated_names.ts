// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2022", "module": "commonjs", "outDir": "out"}, "files": ["a.ts", "b.ts", "c.mts", "d.ts", "e.d.ts"]}
// @filename: a.ts
function require() {}
const exports = {};
class Object {}
export const __esModule = false;
export { require, exports, Object };
// @filename: b.ts
import { x as require } from "./d";
declare function exports(): void;
namespace N { export const require = 1; export class Object {} }
const C = class Object {};
let __esModule = 1;
export { __esModule, C, N };
enum exports2 { A }
export function f(require: number) { return require; }
// @filename: c.mts
function require() {}
class Object {}
export const __esModule = false;
export { require, Object };
// @filename: d.ts
export const x = 1;
class Object {}
// @filename: e.d.ts
declare function require(): void;
declare class Object {}
export declare const __esModule: boolean;
export { require, Object };
