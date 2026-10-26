declare const f: { b: number | undefined; d?: number };
delete f.b;
delete f.d;
export {};
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "exactOptionalPropertyTypes": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true }, "files": ["a.ts"] }
