// A generic alias reached through an import prints with the arguments as written.
import { Component } from "./b";
declare let c: Component;
let n1: number = c;
declare let c2: Component<string>;
let n2: number = c2;

// Inference reads `{ [P in K]: X }[K]` as `X` with `P` replaced by `K`.
interface EM { click: number; scroll: string }
type Ev<K extends keyof EM> = { [P in K]: { readonly name: P; readonly value: EM[P] } }[K];
declare function listen<K extends keyof EM>(e: Ev<K>): Ev<K>;
const clickEvent = listen({ name: "click", value: 1 });
let n3: number = clickEvent;
// @filename: b.ts
export type Component<T = any, U = {}> = { x: T; y: U };
