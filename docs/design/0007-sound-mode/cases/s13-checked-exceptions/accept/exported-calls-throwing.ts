// An exported async function lets a callee's declared exception escape: allowed by default, TSR1302 with exports-declare-throws.
class NotFound extends Error {
  readonly code = "NOT_FOUND";
}
class Timeout extends Error {
  readonly code = "TIMEOUT";
}
/** @throws {NotFound} */
function find(id: string): string {
  if (id === "") throw new NotFound("missing");
  return id;
}
export async function show(id: string): Promise<string> { // @opt-in TSR1302
  return find(id);
}
