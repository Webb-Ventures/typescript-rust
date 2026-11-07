// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"jsx":"preserve","noEmit":true,"lib":["es2015"],"types":[]},"files":["a.tsx"]}
// @filename: a.tsx
type L<C, P> = C extends { defaultProps: infer D } ? P & D : P;
function f<T extends "a" | "b">() {
    const x: L<T, { a?: string }> = {};
    const y: L<T, { a?: string }> = { a: "" };
}
type M<C, P> = C extends { defaultProps: string } ? P & { z: 1 } : P;
function g<T extends "a" | "b">() {
    const x: M<T, { a?: string }> = {};
}
function h<T extends "a" | "b">() {
    const z: L<T, { a?: string }> = 1;
}
function k<T>() {
    const z: L<T, { a?: string }> = {};
}
