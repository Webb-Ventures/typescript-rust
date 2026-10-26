namespace first {
    export enum E { A = 1 }
}
namespace second {
    export enum E { A = 2 }
}
function f(a: number): first.E;
function f(a: string): second.E;
function f(a: number | string): first.E | second.E {
    return first.E.A;
}
function g(a: number): first.E;
function g(a: string): number;
function g(a: number | string): string {
    return "";
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "strictFunctionTypes": false, "noUncheckedIndexedAccess": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true } }
