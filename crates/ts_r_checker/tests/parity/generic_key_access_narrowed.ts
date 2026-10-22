// `obj[key]` with a constant generic key narrows through its union constraint where it
// is the object of an access.
function f4<K extends string>(obj: Record<K, string | undefined>, key: K) {
  if (obj[key]) {
    obj[key].toUpperCase();
  }
  obj[key].toUpperCase();
}
function f5<K extends "a" | "b">(obj: { a: string | null; b: number | null }, key: K) {
  if (obj[key] !== null) {
    obj[key].toString();
  }
}
