var __rewriteRelativeImportExtension = (this && this.__rewriteRelativeImportExtension) || function (path, preserveJsx) {
    if (typeof path === "string" && /^\.\.?\//.test(path)) {
        return path.replace(/\.(tsx)$|((?:\.d)?)((?:\.[^./]+?)?)\.([cm]?)ts$/i, function (m, tsx, d, ext, cm) {
            return tsx ? preserveJsx ? ".jsx" : ".js" : d && (!ext || !cm) ? m : (d + ext + "." + cm.toLowerCase() + "js");
        });
    }
    return path;
};
// @target: es2022
// @module: esnext
// @rewriteRelativeImportExtensions: true
import { a } from "./a.js";
import b from "../b.js";
import "./c.mjs";
export * from "./d.cjs";
export { e } from "./e.d.ts";
import pkg from "pkg/x.ts";
const p = "./x.ts";
import(__rewriteRelativeImportExtension(p));
import("./lit.js");
console.log(a, b, pkg);
