// @module: commonjs
// @target: es2022
// A re-exported default import is assigned; `{ default as x }` is forwarded by a getter.
import b from "./t1";
import f1, { default as f2 } from "./t1";
export { b, f1, f2 };
