import * as dep from "./dep";
import { helper } from "./dep";
import fs = require("./dep");
export * from "./dep";

export { dep };

export const Stub: typeof fs.helper = null as any;
export let count = helper(2);

export function bump() {
  count++;
  return Stub;
}

export function* ids() {
  yield count;
}

export class Greeter {
  greeting = "hi";
  constructor(public name: string, ...rest: string[]) {}
}

export class Plain {
  x = 10;
}

namespace A.B {
  export var x = 1;
  export function f() {
    return x;
  }
}

export enum E {
  a,
  b = 3,
  c,
}

export const { first, second = 2 } = { first: 1 } as { first: number; second?: number };

export const keys = (
  ["a", "b"] as const
).filter((k) => k);
