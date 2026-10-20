// @filename: tsconfig.json
{ "compilerOptions": { "target": "ES2022", "noEmit": true, "strict": false }, "files": ["a.ts"] }
// @filename: a.ts
// A `with` body is not checked (its names are `any`): TS2410 for the statement, TS1300 in an
// async function; the object expression still is checked.
declare var o: { a: number };
with (o) {
    a;
    missing();
    let x: string = 1;
    with (o) { }
}
with (nope) {}
missingAfter;
async function g() {
    with (o) { }
}
