// An override may throw less than the method it overrides.
class NotFound extends Error {
  readonly code = "NOT_FOUND";
}
class Timeout extends Error {
  readonly code = "TIMEOUT";
}
class Base {
  /** @throws {NotFound} */
  get(id: string): string {
    if (id === "") throw new NotFound("missing");
    return id;
  }
}
class Cached extends Base {
  override get(id: string): string {
    return id;
  }
}
export const v: string = new Cached().get("a");
