// @target: es2022
// @module: commonjs
import def from "./def";
export default def + 1;
export class Named {}
export const lazy = () => import("./lazy");
const p = "./x";
export const dyn = () => import(p);
