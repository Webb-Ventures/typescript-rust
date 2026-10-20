// @target: es2022
// @module: esnext
// @rewriteRelativeImportExtensions: true
import { a } from "./a.ts";
import b from "../b.tsx";
import "./c.mts";
export * from "./d.cts";
export { e } from "./e.d.ts";
import pkg from "pkg/x.ts";
const p = "./x.ts";
import(p);
import("./lit.ts");
console.log(a, b, pkg);
