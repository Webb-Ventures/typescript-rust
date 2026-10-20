// NotFound is handled; re-throwing the catch variable passes on only Timeout, which is declared.
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
/** @throws {Timeout} */
export function load(id: string): string | null {
  try {
    return fetchRow(find(id));
  } catch (e) {
    if (e instanceof NotFound) return null;
    throw e;
  }
}
