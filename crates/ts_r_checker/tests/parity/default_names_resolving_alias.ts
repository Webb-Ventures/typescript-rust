// A type parameter default that names a type alias whose union includes the interface
// itself (yaml's `ParsedNode`, typebox's `TTemplateLiteralKind[]`) is not circular: tsc
// defers alias references that need defaults. A default that needs itself directly is.
// @filename: a.ts
export type ParsedNode = Scalar | Seq;
export interface Scalar { value: string }
export interface Seq<T extends ParsedNode = ParsedNode> { items: T[] }
const s: Seq = { items: [{ value: "x" }, { items: [] }] };
const bad: Seq = { items: [1] };

export type Kind = Lit | Tpl;
export interface Lit { lit: string }
export interface Tpl<T extends Kind[] = Kind[]> { kinds: T }
const t: Tpl = { kinds: [{ lit: "a" }, { kinds: [] }] };
const t2: Tpl = { kinds: [2] };

interface Self<T = Self> { x: T }
// @filename: b.ts
// The other order: the alias is resolved first.
type Node2 = Leaf | List2;
interface Leaf { leaf: true }
interface List2<T extends Node2 = Node2> { items: T[] }
const n: Node2 = { items: [{ leaf: true }] };
const n2: List2 = { items: [{ leaf: false }] };
export {};
