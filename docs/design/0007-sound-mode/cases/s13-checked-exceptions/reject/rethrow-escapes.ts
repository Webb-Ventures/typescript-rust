// The catch handles NotFound, but re-throwing passes Timeout on, and the tag only lists NotFound.
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
/** @throws {Timeout} */
function fetchRow(key: string): string {
  if (key === "slow") throw new Timeout("slow");
  return key;
}
/** @throws {NotFound} */
function load(id: string): string {
  try {
    return fetchRow(find(id));
  } catch (e) {
    if (e instanceof NotFound) return "";
    throw e; // @expect TSR1301
  }
}
export const v: string = load("a");
