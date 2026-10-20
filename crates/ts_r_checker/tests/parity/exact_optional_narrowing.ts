// Under exactOptionalPropertyTypes an absent optional property reads as the missing type;
// `=== undefined` / `!== undefined` narrow it like `undefined` (and `ref?.p !== undefined`
// makes `ref` non-nullish).
interface O { p?: string; q?: number | undefined; r: { s?: boolean } }
declare const x: O;
if (x.p !== undefined) { const a: string = x.p; }
if (x.p === undefined) { } else { const b: string = x.p; }
if (x.q !== undefined) { const c: number = x.q; }
if (x.r.s !== undefined) { const d: boolean = x.r.s; }
if (undefined !== x.p) { const e: string = x.p; }
if (x.p != null) { const f: string = x.p; }
if (typeof x.p !== "undefined") { const h: string = x.p; }
if (x.p === undefined) { const i: undefined = x.p; }
function k(o: O) {
  if (o.p === undefined) return;
  const j: string = o.p;
}
declare const y: { p?: string } | undefined;
if (y?.p !== undefined) { const l: string = y.p; }
if (x.p !== undefined) { } else { const n: string = x.p; }
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "exactOptionalPropertyTypes": true, "noEmit": true}, "files": ["a.ts"]}
