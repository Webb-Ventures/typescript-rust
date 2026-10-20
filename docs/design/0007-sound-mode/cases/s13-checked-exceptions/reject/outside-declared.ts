// The function throws a type its tag doesn't list.
class NotFound extends Error {
  readonly code = "NOT_FOUND";
}
class Timeout extends Error {
  readonly code = "TIMEOUT";
}
/** @throws {NotFound} */
function find(id: string): string {
  if (id === "slow") throw new Timeout("slow"); // @expect TSR1301
  if (id === "") throw new NotFound("missing");
  return id;
}
export const v: string = find("a");
