// A bigint key in an object binding pattern cannot index the destructured value (TS2538).
const arr = [1, 2, 3] as const;
const { 0: d } = arr;
const { 0n: f } = arr;
declare const anyValue: any;
const { 1n: g } = anyValue;
export { d, f, g };
