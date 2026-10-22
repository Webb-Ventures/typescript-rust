// Top-level `await` and `for await`: TS1375/TS1431 in a script, TS1378/TS1432 when
// `module`/`target` do not allow them. An operand on the next line is not reported.
// @filename: script.ts
declare const p: Promise<number>;
await p;
for await (const x of [p]) {
  x;
}
// @filename: mod.ts
declare const p: Promise<number>;
await p;
await (p);
declare const dec: any;
@(await dec)
class C {}
for await (const x of [p]) {
  x;
}
export { C };
export let y = await
  p;
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2016", "module": "ES2015", "noEmit": true}, "files": ["script.ts", "mod.ts"]}
