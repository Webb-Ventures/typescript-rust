// `ref?.p === v` narrows `ref` unless `v` may be undefined; type parameters
// do not count as possibly undefined.
type Sorting<T> = { by: keyof T; negate: boolean };
function h<T>(column: keyof T, sorting: Sorting<T> | undefined) {
  return column === sorting?.by ? sorting.negate : false;
}
function h2<T, K extends keyof T>(column: K, sorting: Sorting<T> | undefined) {
  return sorting?.by === column ? sorting.negate : false;
}
function h3<U>(column: U, sorting: { by: U } | undefined) {
  return sorting?.by === column ? sorting.by : false;
}
function h4(column: string, sorting: { by: string } | undefined) {
  return sorting?.by === column ? sorting.by : false;
}
function h5(column: unknown, sorting: { by: unknown } | undefined) {
  return sorting?.by === column ? sorting.by : false;
}
function h6<U>(column: U | undefined, sorting: { by: U } | undefined) {
  return sorting?.by === column ? sorting.by : false;
}
function h7(column: any, sorting: { by: unknown } | undefined) {
  return sorting?.by === column ? sorting.by : false;
}
function h8<U extends string | undefined>(column: U, sorting: { by: unknown } | undefined) {
  return sorting?.by === column ? sorting.by : false;
}
