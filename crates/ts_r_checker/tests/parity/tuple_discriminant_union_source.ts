// A tuple with a union discriminant element relates to a union of tuples member by member.
type A = ["a", number] | ["b", number] | ["c", string];
declare const s1: ["a" | "b", number];
const a1: A = s1;
declare const b: "a" | "b" | "c";
const a2: A = b === "a" || b === "b" ? [b, 1] : ["c", ""];
declare const s4: ["a" | "b", number, boolean];
const a4: A = s4;
