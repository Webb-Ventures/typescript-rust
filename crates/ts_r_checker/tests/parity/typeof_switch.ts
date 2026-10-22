// `switch (typeof x)`: template cases, a case after one that already matched, `object`
// removed by "object" alone, and exhaustiveness (no TS2366).
declare function assertNever(x: never): never;
declare function assertNumber(x: number): void;
declare function assertObject(x: object): void;
declare function assertStringOrNumber(x: string | number): void;
declare function assertBooleanOrObject(x: boolean | object): void;
type Basic = number | boolean | string | symbol | object | Function | undefined;
function testUnion(x: Basic) {
  switch (typeof x) {
    case `number`: assertNumber(x); return;
    case "boolean": return;
    case "function": return;
    case "symbol": return;
    case `object`: assertObject(x); return;
    case "string": return;
    case "undefined": return;
  }
  assertNever(x);
}
function fallThrough(x: string | number | boolean | object) {
  switch (typeof x) {
    case "number":
      assertNumber(x);
    case "string":
      assertStringOrNumber(x);
      break;
    default:
      assertObject(x);
    case "number":
    case "boolean":
      assertBooleanOrObject(x);
      break;
  }
}
function narrowingNarrows(x: {} | undefined) {
  switch (typeof x) {
    case "number": assertNumber(x); return;
    case "number": assertNever(x); return;
    case "string": assertNever(x); return;
  }
}
type L = (x: number) => string;
type R = { x: string; y: number };
function exhaustive(x: number | string | L | R): string {
  switch (typeof x) {
    case "number": return x.toString(2);
    case "string": return x;
    case "function": return x(42);
    case "object": return x.x;
  }
}
function notExhaustive(x: number | string | L | R): string {
  switch (typeof x) {
    case "number": return "";
    case "function": return "";
  }
}
function objectAlone(c: object | string) {
  switch (typeof c) {
    case "object": return;
    default: const n: string = c;
  }
}
