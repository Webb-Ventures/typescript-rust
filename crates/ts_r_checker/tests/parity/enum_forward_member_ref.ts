enum E {
    A = B,
    C = E.D,
    F = E["G"],
    B = 1,
    H = E.B,
}
enum E {
    D = 2,
    G = 3,
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
