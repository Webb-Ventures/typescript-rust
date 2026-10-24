// @filename: tsconfig.json
{"compilerOptions": {"strict": false, "noImplicitAny": true, "target": "ES2023", "module": "ESNext", "noEmit": true}, "files": ["a.ts"]}
// @filename: a.ts
// A spread of an optional property keeps the left side's widening `null`, which widens
// and is reported at the left property.
declare const flag: boolean;
function make() {
  if (flag) {
    return {};
  }
  return { c: "foo", p: "bar" };
}
const bar = {
  p: null,
  ...make(),
};
const baz = { p: null, ...(flag ? { p: 1 } : {}) };
