// Contextual property types from an intersection come from each object constituent, an
// `any` property contributing nothing (`unknown`): `{ ml: any } & { ml: "edge" }` keeps
// `"edge"` a literal, for properties, tuple elements and returned object literals.
declare function take(a: { type: "foo" | "bar" } & { type: any }): void;
take({ type: "foo" });
const x: { ml: any } & { ml: "edge" } = { ml: "edge" };
const a: [any] & [1] = [1];
const b: any[] & 1[] = [1, 1];
const d: (() => { a: 1 }) & (() => { a: any }) = () => ({ a: 1 });
const e: { k: any } & { [s: string]: "v" } = { k: "v", j: "v" };
const n1: { ml: any } & { ml: "edge" } = { ml: "other" };
const n2: [any] & [1] = [2];
