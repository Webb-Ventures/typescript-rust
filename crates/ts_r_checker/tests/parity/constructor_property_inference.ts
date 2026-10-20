// trpc `redirect.ts` (`public readonly args;` then `this.args = [url.toString(), redirectType]
// as const` in the constructor, spread into `redirect(...error.args)`): under noImplicitAny an
// unannotated, uninitialised property takes the type `this.p` has where the constructor (for
// a static one, a static block) returns; otherwise TS7008. A reference in code a `never` call
// makes unreachable keeps its declared type.
type RT = "push" | "replace";
declare function redirect(url: string, type?: RT): never;
class E {
  public readonly args;
  public other;
  maybe;
  never;
  onlyNull;
  inMethod;
  viaReturn;
  static st;
  static fromBlock;
  "quoted";
  0;
  constructor(url: string, t?: RT) {
    this.args = [String(url), t] as const;
    if (t) this.other = 1;
    else this.other = "a";
    if (t) this.maybe = true;
    this.onlyNull = null;
    this["quoted"] = [1];
    this[0] = { a: 1 };
    if (url) {
      this.viaReturn = 1;
      return;
    }
    this.viaReturn = "s";
  }
  m() {
    this.inMethod = 1;
  }
  static {
    this.fromBlock = 1;
  }
}
class NoCtor {
  x;
}
declare class Ambient {
  y;
  private z;
}
declare const e: E;
function f() {
  redirect(...e.args);
  const n: null = e.args;
}
const n2: null = e.other;
const n3: null = e.maybe;
const n4: null = e.never;
const n5: null = e.onlyNull;
const n6: null = e.viaReturn;
const n7: null = E.fromBlock;
const n8: null = e.quoted;
const n9: null = e[0];
export {};
