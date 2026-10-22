enum E { a, b }
const O = { a: "a" };
declare let x: number;
export const r = [
    E.a as const,
    E["b"] as const,
    (E).a as const,
    O.a as const,
    x as const,
    -1 as const,
    +1 as const,
    -1n as const,
    -x as const,
    `t${x}` as const,
    (true) as const,
    <const>"s",
];
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
