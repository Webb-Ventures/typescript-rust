// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "target": "ES2022", "module": "ESNext"}, "files": ["a.ts", "m.ts"]}
// @filename: a.ts
// `yield` names in generators are strict-mode reserved words (binder), not parse errors;
// `await` names in async functions are TS1359 and do not stop semantic checking.
async function * f5(yield) {}
const a = async function * yield() {};
const b = async function * await() {};
function* g() { const c = function yield() {}; }
async function h() { const d = function await() {}; function await() {} }
class C { *m() { let yield = 1; } }
let n: number = "not checked away";
// @filename: m.ts
export function* g() {
    const yield = 4;
}
export async function f() {
    const await = 3;
}
