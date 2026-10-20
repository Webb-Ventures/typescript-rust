// @filename: tsconfig.json
{
  "compilerOptions": {
    "strict": true,
    "strictBuiltinIteratorReturn": false,
    "target": "ES2023",
    "lib": ["ES2023"],
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "moduleDetection": "force",
    "noEmit": true
  },
  "files": ["a.ts"]
}
// @filename: a.ts
// Without `strictBuiltinIteratorReturn`, the lib's `BuiltinIteratorReturn` is `any`.
export const m: string = new Set<number>().values().next();
export const k: string = new Map<string, number>().keys().next().value;
