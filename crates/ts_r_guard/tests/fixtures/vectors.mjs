// Test vectors per `Cases` property: values `is` must accept (and `parse` return as is), and
// values it must reject with the exact `ParseError` message (`null`: any `ParseError`).
const addr = { street: "s" };
const user = { id: 1, name: "a", role: "admin", tags: [], manager: null, address: addr };
// Deeper than max-depth (512), and cyclic: a ParseError, never a stack overflow.
let deep = { value: 1, children: [] };
for (let i = 0; i < 600; i++) deep = { value: 1, children: [deep] };
const cyclic = { value: 1, children: [] };
cyclic.children.push(cyclic);

export default {
  user: {
    ok: [
      user,
      { ...user, email: "e", role: "member", tags: ["x"], manager: { ...user, id: 2, address: { street: "t", zip: "1" } }, extra: true },
    ],
    bad: [
      [{ ...user, id: "1" }, "Invalid User: $.id: expected number, received string"],
      [{ ...user, role: "boss" }, 'Invalid User: $.role: expected "admin" | "member", received string'],
      [{ ...user, email: undefined }, "Invalid User: $.email: expected string, received undefined"],
      [{ id: 1, role: "admin", tags: [1], manager: null, address: {} }, "Invalid User: $.name: expected string, received missing (+2 more)"],
      [null, "Invalid User: $: expected object, received null"],
      [[], "Invalid User: $: expected object, received array"],
      [{ ...user, manager: { id: 2 } }, "Invalid User: $.manager.name: expected string, received missing (+4 more)"],
    ],
  },
  event: {
    ok: [{ type: "click", x: 1, y: 2 }, { type: "key", key: "a" }],
    bad: [
      [{ type: "key", key: 1 }, "Invalid Event: $.key: expected string, received number"],
      [{ type: "drag" }, 'Invalid Event: $.type: expected "click" | "key", received string'],
      ["click", "Invalid Event: $: expected object, received string"],
    ],
  },
  page: {
    ok: [{ items: [user], next: null }, { items: [], next: "n" }],
    bad: [[{ items: [{ ...user, tags: "x" }], next: null }, "Invalid Page<User>: $.items[0].tags: expected array, received string"]],
  },
  pair: {
    ok: [["a"], ["a", 1]],
    bad: [
      [[], "Invalid Pair: $: expected tuple of 1 to 2 elements, received 0 elements"],
      [["a", "b"], "Invalid Pair: $[1]: expected number, received string"],
      [["a", 1, 2], "Invalid Pair: $: expected tuple of 1 to 2 elements, received 3 elements"],
    ],
  },
  scores: { ok: [{}, { a: 1 }], bad: [[{ a: "x" }, "Invalid Scores: $.a: expected number, received string"]] },
  userKey: {
    ok: ["user_1", "user_1.5", "user_-3", "user_1e3"],
    bad: [
      ["user_", "Invalid UserKey: $: expected `user_${number}`, received string"],
      ["user_x", "Invalid UserKey: $: expected `user_${number}`, received string"],
      [5, "Invalid UserKey: $: expected `user_${number}`, received number"],
    ],
  },
  color: { ok: [0, 5, 6], bad: [[1, "Invalid Color: $: expected 0 | 5 | 6, received number"]] },
  dir: { ok: ["UP", "DOWN"], bad: [["up", 'Invalid Dir: $: expected "UP" | "DOWN", received string']] },
  derived: { ok: [{ id: "a", extra: true }], bad: [[{ extra: true }, "Invalid Derived: $.id: expected string, received missing"]] },
  both: { ok: [{ id: "a", n: 1 }], bad: [[{ id: "a" }, "Invalid Both: $.n: expected number, received missing"]] },
  tree: {
    ok: [{ value: 1, children: [{ value: 2, children: [] }] }],
    bad: [
      [{ value: 1, children: [{ value: "2", children: [] }] }, "Invalid Tree: $.children[0].value: expected number, received string"],
      [deep, null],
      [cyclic, null],
    ],
  },
  withRest: {
    ok: [["a"], ["a", 1, 2]],
    bad: [
      [["a", "b"], "Invalid WithRest: $[1]: expected number, received string"],
      [[], "Invalid WithRest: $: expected tuple of at least 1 elements, received 0 elements"],
    ],
  },
  maybe: { ok: [{ value: "x" }, { value: undefined }], bad: [[{}, "Invalid Maybe: $.value: expected string | undefined, received missing"]] },
  partial: { ok: [{}, { id: "a" }], bad: [[{ id: 1 }, "Invalid Partial<Base>: $.id: expected string, received number"]] },
  pick: { ok: [{ id: 1, name: "a" }], bad: [[{ id: 1 }, 'Invalid Pick<User, "id" | "name">: $.name: expected string, received missing']] },
  omit: { ok: [{ id: "x" }], bad: [[{}, 'Invalid Omit<Derived, "extra">: $.id: expected string, received missing']] },
  date: { ok: [new Date(0)], bad: [["2020", "Invalid Date: $: expected Date, received string"]] },
  map: {
    ok: [new Map([["a", 1]]), new Map()],
    bad: [[new Map([["a", "b"]]), "Invalid Map<string, number>: $.a: expected number, received string"]],
  },
  set: { ok: [new Set(["active"])], bad: [[new Set(["x"]), 'Invalid Set<Status>: $[0]: expected "active" | "paused", received string']] },
  status: { ok: ["paused"], bad: [["x", 'Invalid Status: $: expected "active" | "paused", received string']] },
  nsAddress: { ok: [addr], bad: [[{ street: "s", zip: 1 }, "Invalid other.Address: $.zip: expected string, received number"]] },
  shared: { ok: [addr], bad: [[{}, "Invalid Shared: $.street: expected string, received missing"]] },
  literalNum: { ok: [-1, 2], bad: [[1, "Invalid -1 | 2: $: expected -1 | 2, received number"]] },
  big: { ok: [1n], bad: [[1, "Invalid bigint: $: expected bigint, received number"]] },
  nested: {
    ok: [{ a: [{ b: ["x"] }] }],
    bad: [[{ a: [{ b: [1] }] }, "Invalid { a: { b: string[] }[] }: $.a[0].b[0]: expected string, received number"]],
  },
  keys: { ok: ["id"], bad: [["x", 'Invalid keyof Base: $: expected "id", received string']] },
  indexed: { ok: ["admin"], bad: [["x", 'Invalid User["role"]: $: expected "admin" | "member", received string']] },
  nonNull: { ok: ["a"], bad: [[null, "Invalid NonNullable<string | null | undefined>: $: expected string, received null"]] },
  excl: { ok: ["a", "c"], bad: [["b", 'Invalid Exclude<"a" | "b" | "c", "b">: $: expected "a" | "c", received string']] },
  unknownField: { ok: [{ data: 1 }, { data: undefined }], bad: [[{}, "Invalid { data: unknown }: $.data: expected unknown, received missing"]] },
  readonlyArr: { ok: [[1]], bad: [[["1"], "Invalid readonly number[]: $[0]: expected number, received string"]] },
  bools: { ok: [true, false], bad: [[0, "Invalid boolean: $: expected boolean, received number"]] },
  colorRed: { ok: [0], bad: [[5, "Invalid Color.Red: $: expected 0, received number"]] },
  item: {
    ok: [{ sku: "a", price: { amount: 1 } }],
    bad: [[{ sku: "a", price: {} }, "Invalid Item: $.price.amount: expected number, received missing"]],
  },
  quoted: {
    ok: [{ "content-type": "x", 1: 2 }],
    bad: [[{ "content-type": 1, 1: 2 }, 'Invalid { "content-type": string; 1: number }: $["content-type"]: expected string, received number']],
  },
};
