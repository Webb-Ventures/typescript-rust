// Property elaboration: a literal property value widens as in its object literal unless the
// target property type has literals of its kind (booleans keep their literal).
export {};
const f1: () => "foo" = () => "bar";
const f2: () => number = () => "bar";
const f3: () => boolean | undefined = () => "bar";
const o1: { u?: boolean } = { u: "true" };
const o2: { u: "a" | "b" } = { u: "c" };
const o3: { u: 1 | 2 } = { u: 3 };
const o4: { u: number } = { u: "x" };
const o5: { u?: boolean; v: string } = { u: "true", v: "" };
