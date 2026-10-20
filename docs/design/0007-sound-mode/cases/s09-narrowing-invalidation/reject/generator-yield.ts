// The caller runs between yields and can change o.v.
function* lengths(o: { v: string | null }): Generator<number, void, unknown> {
  if (o.v !== null) {
    yield 0;
    yield o.v.length; // @expect TSR0901
  }
}
export const first: IteratorResult<number, void> = lengths({ v: "a" }).next();
