// any inside a type argument and as a generic default.
function keys(record: Record<string, any>): string[] { // @expect TSR0101
  return Object.keys(record);
}
class Box<T = any> { // @expect TSR0101
  constructor(readonly value: T) {}
}
const box = new Box(1);
export const k: string[] = keys({ a: box.value });
