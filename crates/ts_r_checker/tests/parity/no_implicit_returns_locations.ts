declare function cb(f: () => unknown): void;
cb(function () { if (Math.random()) return 1; });
cb(function named() { if (Math.random()) return 1; });
const o = { p: function () { if (Math.random()) return 1; } };
let v; v = function () { if (Math.random()) return 1; };
class K { x = () => { if (Math.random()) return 1; }; y = function () { if (Math.random()) return 1; }; static z() { if (Math.random()) return 1; } }
cb(async () => { if (Math.random()) return 1; });
cb(async function () { if (Math.random()) return 1; });
function* gen() { if (Math.random()) return 1; yield 2; }
function* gen2() { yield 2; }
async function* ag() { if (Math.random()) return 1; }
function ann(): Promise<number> | undefined { if (Math.random()) return Promise.resolve(1); }
async function ann2(): Promise<number | undefined> { if (Math.random()) return 1; }
function ann3(): number | void { if (Math.random()) return 1; }
async function ann4(): Promise<void | number> { if (Math.random()) return 1; }
function ann5(): any { if (Math.random()) return 1; }
function ann6(): never | undefined { if (Math.random()) throw 1; }
function ann7<T>(x: T): T | undefined { if (Math.random()) return x; }
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleDetection": "force", "noEmit": true, "noImplicitReturns": true }, "files": ["a.ts"] }
