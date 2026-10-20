// Array destructuring reads an index too.
function headLength(list: readonly string[]): number {
  const [head] = list;
  return head.length; // @expect TSR0401
}
export const h: number = headLength(["a"]);
