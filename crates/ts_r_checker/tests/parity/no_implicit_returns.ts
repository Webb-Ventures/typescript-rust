function g1(): number { return; }
function g2(x: boolean): number { if (x) return 1; return; }
function g3(x: boolean): number | undefined { if (x) return 1; return; }
function g4(x: boolean) { if (x) return 1; return; }
function g5(x: boolean): unknown { if (x) return 1; return; }
function g6(x: boolean): unknown { }
function g7(x: boolean): number { if (x) return 1; }
function g8(x: boolean): string | undefined { }
const a1 = (x: boolean) => { if (x) return 1; };
const a2 = function (x: boolean) { if (x) return 1; };
const a3 = async (x: boolean) => { if (x) return 1; };
class C { m(x: boolean) { if (x) return 1; } get p() { if (Math.random()) return 1; } }
const o = { m(x: boolean) { if (x) return 1; }, n: (x: boolean) => { if (x) return 1; } };
function g9(x: boolean) { if (x) return 1; throw 0; }
function* g10(x: boolean) { if (x) return 1; }
function g11(x: boolean) { if (x) return 1 as any; }
function g12(x: boolean) { if (x) return; }
function g13(x: boolean): undefined { if (x) return; }
function g14(x: boolean) { if (x) return null; }
function g15(x: boolean) { if (x) return undefined as void | undefined; }
declare function cb(f: () => number | undefined): void;
cb(() => { if (Math.random()) return 1; });
declare function cb2(f: () => any): void;
cb2(() => { if (Math.random()) return 1; });
declare function cb3(f: () => void): void;
cb3(() => { if (Math.random()) return 1; });
export default function (x: boolean) { if (x) return 1; }
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleDetection": "force", "noEmit": true, "noImplicitReturns": true }, "files": ["a.ts"] }
