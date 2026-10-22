class C {
    x1(a: number, callback: (x: "hi") => number): number;
    x1(a: number, callback: (x: string) => number) { return 1; }
}
function f(a: number, callback: (x: "hi") => number): number;
function f(a: number, callback: (x: string) => number) { return 1; }
class D {
    x1(a: number, callback: (x: "hi") => number);
    x1(a: number, callback: (x: string) => number) { }
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": false, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
