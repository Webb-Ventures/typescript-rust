declare var dec: any;
@dec export @dec class C6 {}
// @filename: b.ts
declare var dec: any;
@dec export default @dec class C8 {}
// @filename: tsconfig.json
{"compilerOptions": {"target":"esnext","module":"esnext","noEmit":true}, "files": ["a.ts","b.ts"]}
