// Spreading a union whose members include an intersection distributes over the members
// (each spread on its own); the result is a union of object types, not unresolved.
interface Shape {
  code: number;
}
interface Base {
  id?: string | null;
  jsonrpc?: "2.0";
}
interface ErrorResponse<E extends Shape = Shape> extends Base {
  error: E;
}
export function g<E extends Shape>(
  response: ErrorResponse<E> | ({ id: string } & ErrorResponse<E>),
  extra: { tag: 1 } | ({ n: 2 } & { m: 3 }),
) {
  const e = { ...response, flag: true };
  const a: E = e.error;
  const b: string | null | undefined = e.id;
  const c: boolean = e.flag;
  const d: number = e.error;
  const x = { ...extra, ...response };
  const k: string = x.error.code;
  return e;
}
export const r: string = g<Shape>({ error: { code: 1 } }, { tag: 1 }).flag;
