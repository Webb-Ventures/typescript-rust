// @filename: tsconfig.json
{"compilerOptions": {"strict": false, "noImplicitAny": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "noEmit": true}, "files": ["a.ts"]}
// @filename: a.ts
var a = null;
let b = undefined;
var c = [null, undefined];
var d = { x: null, y: 1 };
var [e, f] = [null, undefined];
var { g } = { g: null };
function h(p = null) { return p; }
function k() { return null; }
const m = function named() { return undefined; };
class C {
    prop = null;
    method() { return null; }
}
function* gen() { yield null; }
async function asy() { return null; }
var ok1 = null as string;
var ok2: string = null;
var ok3 = [];
