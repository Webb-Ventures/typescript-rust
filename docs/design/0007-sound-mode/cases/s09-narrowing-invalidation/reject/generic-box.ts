// A generic property narrowing dropped by a call.
interface Box<T> {
  item: T | undefined;
}
function take<T>(box: Box<T>, done: () => void): T | undefined {
  if (box.item !== undefined) {
    done();
    const t: T = box.item; // @expect TSR0901
    return t;
  }
  return undefined;
}
export const v: number | undefined = take({ item: 1 }, () => undefined);
