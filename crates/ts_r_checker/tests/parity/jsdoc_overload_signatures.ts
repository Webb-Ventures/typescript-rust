// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "noEmit": true, "allowJs": true, "checkJs": true, "types": []}, "files": ["a.js"]}
// @filename: a.js
/**
 * @overload
 * @param {string} x
 * @returns {string}
 */
/**
 * @overload
 * @param {number} x
 * @returns {number}
 */
/**
 * @param {string | number} x
 */
function f(x) { return x; }
/** @type {boolean} */
const b = f(1);
f(true);
/**
 * @template T
 * @overload
 * @param {T[]} a
 * @returns {T}
 */
/** @param {unknown} a */
function g(a) { return a; }
/** @type {boolean} */
const c = g([1]);
class C {
  /**
   * @overload
   * @param {string} x
   * @returns {void}
   */
  /** @param {string} x */
  m(x) {}
}
new C().m(1);
