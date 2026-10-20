// Types the node test validates: every property of `Cases` is one `parse<T>` type argument,
// every property of `Unsupported` must be rejected with a reason (TSR1501).
import type { Address, Status } from "./other";
import * as other from "./other";
import type { Shared } from "./reexport";
import type { Item } from "typed-pkg";

export interface User {
  id: number;
  name: string;
  email?: string;
  role: "admin" | "member";
  tags: string[];
  manager: User | null;
  address: Address;
}

export type Event =
  | { type: "click"; x: number; y: number }
  | { type: "key"; key: string };

export interface Page<T> {
  items: T[];
  next: string | null;
}

export type Pair = [string, number?];
export type Scores = Record<string, number>;
export type UserKey = `user_${number}`;
export enum Color {
  Red,
  Green = 5,
  Blue,
}
export enum Dir {
  Up = "UP",
  Down = "DOWN",
}
type Base = { id: string };
interface Derived extends Base {
  extra: boolean;
}
type Both = Base & { n: number };
type Tree = { value: number; children: Tree[] };
type WithRest = [string, ...number[]];
type Maybe<T = string> = { value: T | undefined };

interface Cases {
  user: User;
  event: Event;
  page: Page<User>;
  pair: Pair;
  scores: Scores;
  userKey: UserKey;
  color: Color;
  dir: Dir;
  derived: Derived;
  both: Both;
  tree: Tree;
  withRest: WithRest;
  maybe: Maybe;
  partial: Partial<Base>;
  pick: Pick<User, "id" | "name">;
  omit: Omit<Derived, "extra">;
  date: Date;
  map: Map<string, number>;
  set: Set<Status>;
  status: Status;
  nsAddress: other.Address;
  shared: Shared;
  literalNum: -1 | 2;
  big: bigint;
  nested: { a: { b: string[] }[] };
  keys: keyof Base;
  indexed: User["role"];
  nonNull: NonNullable<string | null | undefined>;
  excl: Exclude<"a" | "b" | "c", "b">;
  unknownField: { data: unknown };
  readonlyArr: readonly number[];
  bools: boolean;
  colorRed: Color.Red;
  quoted: { "content-type": string; 1: number };
  item: Item;
}

class Klass {
  x = 1;
}

interface Unsupported {
  fn: () => void;
  anyT: any;
  promise: Promise<string>;
  cls: Klass;
  method: { m(): void };
  cond: string extends number ? 1 : 2;
  query: typeof globalThis;
  brand: string & { __brand: "Id" };
  never: never;
  missing: NotDeclaredAnywhere;
}
