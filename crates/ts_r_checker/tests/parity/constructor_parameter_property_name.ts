class Foo {
  constructor(public constructor: string) {}
}
class Bar {
  constructor(readonly constructor: number, private x: number) {}
}
export {};
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
