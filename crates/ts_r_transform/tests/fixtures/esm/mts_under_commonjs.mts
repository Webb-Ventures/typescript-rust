// @target: es2022
// @module: commonjs
// A .mts file stays an ES module under `module: commonjs`, but gets "use strict".
import type { A } from "./a";
export type B = A;
export const x = 1;
