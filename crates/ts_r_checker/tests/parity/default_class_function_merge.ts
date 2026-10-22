// @filename: a.ts
// A default-exported class merged with default-exported functions: TS2813/TS2814.
export default function bar() { }
export default class C {}
// @filename: b.ts
export default class D {}
export default function f(): void;
export default function f() {}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "esnext", "noEmit": true }, "files": ["a.ts", "b.ts"] }
