// A type predicate keeps a union member that is only assignable (not a subtype) to the
// predicate's type: `isFunction(x)` with `x is (...args: any[]) => any` keeps
// `(source: string) => number`, whose parameter `any` does not relate as a subtype.
declare function isFunction(value: any): value is (...args: any[]) => any;
declare class Box<T> { value: T }
export function m4<T, R>(selector?: (source: Box<T>) => Box<R>) {
  if (isFunction(selector)) {
    const r: number = selector;
  }
}
export function m5(selector?: (source: string) => number) {
  if (isFunction(selector)) {
    const r: boolean = selector;
  }
}
export function m6(selector: ((source: string) => number) | string) {
  if (isFunction(selector)) {
    const r: boolean = selector;
  }
}
declare function isObj(value: any): value is { [k: string]: any };
export function m7(o?: { a: number }) {
  if (isObj(o)) {
    const r: boolean = o;
  }
}
