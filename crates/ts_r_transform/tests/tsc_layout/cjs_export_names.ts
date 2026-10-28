// @module: commonjs
// @target: es2022
// A name exported twice is set to `void 0` once.
const foo = 1;
export { foo, foo as bar };
export { foo as baz, foo };
export { x as default } from "./a";
