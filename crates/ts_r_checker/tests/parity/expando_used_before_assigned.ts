// @filename: tsconfig.json
{"compilerOptions": {"noEmit": true, "allowJs": true, "checkJs": true, "strict": true, "target": "ES2022", "module": "ESNext"}, "files": ["a.ts", "b.js"]}
// @filename: a.ts
// A property declared by assignment (`f.p = v`) read in the same flow container where
// no assignment through that reference reaches it: TS2565.
export function f() {}
f.read = f.later;
f.later = 1;
f.after;
f.after = 2;
// @filename: b.js
class C {
  static blah1 = 123;
}
C.blah2 = 456;

class D extends C {
  static {
    console.log(super.blah1);
    console.log(super.blah2);
  }
}

function f() {}
f.read = f.later;
f.later = 1;
f.after = 2;
f.after;
if (Math.random()) {
  f.branch = 1;
}
f.branch;
function g() {
  return f.later + f.after;
}
export { C, D, g };
