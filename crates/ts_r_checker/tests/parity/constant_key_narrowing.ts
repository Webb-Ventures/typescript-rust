// obj[key] narrows when key is a const, a parameter or a let that is never assigned.
type Order = "ASC" | "DESC" | { order: "ASC" | "DESC"; nulls?: "FIRST" | "LAST" };
declare const orderBys: Record<string, Order>;
export const r = Object.keys(orderBys).map((columnName) => {
  const v =
    typeof orderBys[columnName] === "string"
      ? orderBys[columnName]
      : orderBys[columnName].order + " " + orderBys[columnName].nulls;
  return v;
});
export function f(key: string, o: Record<string, string | number>) {
  if (typeof o[key] === "string") {
    const n: number = o[key];
  }
  key = "x";
}
export function g(key: string, o: Record<string, string | number>) {
  if (typeof o[key] === "string") {
    const n: number = o[key];
  }
}
export function h(o: Record<string, string | number>) {
  let key = "a";
  if (typeof o[key] === "string") {
    const n: number = o[key];
  }
  const k2 = "b";
  if (typeof o[k2] === "string") {
    const n: number = o[k2];
    const m: number = o.b;
  }
}
