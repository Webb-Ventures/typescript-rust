// @target: es2022
// @module: commonjs
import fs from "fs";
import * as path from "path";
import { a, b as bb } from "./d";
import def, { c } from "./e";
import def2, * as ns2 from "./f";
import "./side";
import type { T } from "./t";
export * from "./star";
export * as nsx from "./nsx";
export { x, y as yy } from "./x";
export { default } from "./dd";
export { default as dd2, z } from "./dd2";
export const K = 1, L = 2;
export let mut = 0;
export function fn() { mut++; return a + bb + c + def + def2 + fs + path + ns2; }
export class Cls {}
export default function () {}
const local = 1;
export { local, local as alias };
export { a as reexp };
mut = 5;
[mut] = [1];
({ mut } = { mut: 2 });
for (mut of [1]) {}
a();
a.b();
bb`t`;
export enum En { A }
export namespace NS { export const q = 1; }
let late = 1;
export { late };
import("./lazy");
export const { d1, d2: [d3] } = { d1: 1, d2: [2] };
