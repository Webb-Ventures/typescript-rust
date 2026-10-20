// trpc `createResolver` (`_def: AnyProcedure["_def"] = { ..._def, type: _defIn.type }`) and
// `wsClient` state updates: an object whose discriminant is a union relates to a
// discriminated union when every value selects members it relates to on its other
// properties. Failures elaborate against the members some value selects.
type T = { type: "a"; x: number } | { type: "b"; x: number } | { type: "c"; x: number } | { type: "a"; x: number; y?: string };
declare const s: { type: "a" | "b" | "c"; x: number; z: boolean };
const t: T = s;
const u: T = { ...s };
declare const s2: { type: "a" | "b" | "d"; x: number };
const t2: T = s2;
declare const s3: { type: "a" | "b"; x: string };
const t3: T = s3;
type U = { k: "a"; v: string } | { k: "b"; v: number };
declare const s4: { k: "a" | "b"; v: string };
const t4: U = s4;
export {};
