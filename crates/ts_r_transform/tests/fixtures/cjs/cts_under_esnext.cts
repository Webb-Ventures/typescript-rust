// @target: es2022
// @module: esnext
// A .cts file is CommonJS whatever `module` says (ecosystem: @eslint/* dist/cjs/types.cts).
import type { A } from "./a";
export interface B { a: A }
export const x = 1;
