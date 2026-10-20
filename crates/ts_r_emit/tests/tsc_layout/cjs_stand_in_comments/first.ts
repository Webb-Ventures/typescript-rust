import dep from "./dep"; // first statement at offset 0
import * as all from "./dep"; // second
export const x = dep + all.default; // exported
export interface I {}
export type T = I; // erased
export namespace N { // namespace
    export const y = 1;
}
