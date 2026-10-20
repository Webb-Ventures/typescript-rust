// trpc `vendor/is-plain-object.ts` (`if (isObject(o) === false) return false`): a condition
// compared with a boolean literal narrows as the condition itself (negated for `false`);
// references keep equality narrowing (`x.ok === true` is a discriminant).
declare function isObj(o: unknown): o is Record<string, unknown>;
declare function isStr(o: unknown): o is string;
function f(o: unknown) {
  if (isObj(o) === false) return false;
  const a: null = o;
}
function g(o: unknown) {
  if (isStr(o) === true) {
    const b: null = o;
  }
  if (isStr(o) !== false) {
    const c: null = o;
  }
  if (false == isStr(o)) {
  } else {
    const d: null = o;
  }
  if (isStr(o) !== true) {
    const e: null = o;
  }
}
function h(x: { ok: true; v: 1 } | { ok: 1; v: 2 }) {
  if (x.ok === true) {
    const v: null = x.v;
  }
}
export {};
