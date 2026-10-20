// Every checked exception is caught, so the exported function throws nothing.
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
export function findOrNull(id: string): string | null {
  try {
    return find(id);
  } catch {
    return null;
  }
}
