// @filename: a.js
export class C {
  name = "CompileDiagnostic";
  /** @param {[number, number] | undefined} position */
  constructor(position) {
    if (position) {
      this.position = position;
    }
    this.always = 1;
    if (position) return;
    this.late = "s";
  }
  m() { this.always = 2; }
}
class D {
  constructor() { this.n = null; if (Math.random()) this.n = 1; }
}
/** @type {string} */
var c1 = new C([1, 2]).position;
/** @type {string} */
var c2 = new C([1, 2]).always;
/** @type {string} */
var c3 = new C([1, 2]).late;
/** @type {string} */
var c4 = new D().n;
// @filename: tsconfig.json
{"compilerOptions": {"target":"es2022","allowJs":true,"checkJs":true,"strict":true,"noEmit":true}, "files": ["a.js"]}
