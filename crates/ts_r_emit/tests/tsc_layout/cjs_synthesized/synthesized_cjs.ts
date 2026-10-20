/// <reference lib="es2022" />
export interface I { x: number }
import fs = require("fs"); // the fs module
export { readFileSync } from "fs";
declare function dec(...a: any[]): any;
@dec
export class C {
    @dec m() {}
    @dec @dec p: number;
}
export const tagged = String.raw`a${1}`;
const xy = 1, o = { xy };
export enum E { A } // trailing enum comment
export const g = [1].map(async x => x);
export default xy; // the default
