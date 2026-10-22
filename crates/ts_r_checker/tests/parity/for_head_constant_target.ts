const v = 1;
for (v of [1]) { }
for (v in {}) { }
function f() {}
for (f of [1]) { }
class C {}
for (C in {}) { }
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
