// @target: es2015
// @module: commonjs
export declare let a: { x: number };
export declare const m: number;
a.x;
export const e = m;
function f() { return a; }
namespace x { export class c {} }
export import xc = x.c;
export var cProp = new xc();
import q = require("./q");
export import r = require("./r");
export import r2 = r;
var y = r.x, z = r2.x;
xc.call; (0, r)(); r2();
