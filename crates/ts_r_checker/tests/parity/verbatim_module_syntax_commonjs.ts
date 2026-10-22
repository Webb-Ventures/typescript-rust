// @filename: types.ts
// Under verbatimModuleSyntax, a CommonJS file has no ES exports of values.
export interface Date { day: number }
export class C {}
// @filename: main.ts
import type { Date } from "./types";
import("./types");
export const x = 1;
export interface I {}
export namespace Types { export type T = number }
export namespace Values { export const y = 1 }
export declare const z: number;
const d: Date = { day: 1 };
// @filename: def.ts
interface I {}
export default I;
// @filename: eq.ts
interface I {}
export = I;
// @filename: eq2.ts
import type { C } from "./types";
export = C;
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "commonjs", "verbatimModuleSyntax": true, "noEmit": true }, "files": ["types.ts", "main.ts", "def.ts", "eq.ts", "eq2.ts"] }
