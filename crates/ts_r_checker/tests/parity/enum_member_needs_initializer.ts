enum A {
    X = "".length,
    Y = X,
    Z
}
enum B {
    P = "s",
    Q
}
declare const n: number;
enum C {
    R = n,
    S
}
enum D {
    T = A.Y,
    U,
    V = B.P,
    W
}
const enum E {
    X = 1,
    Y = X + 1,
    Z
}
export {};
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
