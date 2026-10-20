// An override that can throw something the base method doesn't declare.
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
class Remote extends Base {
  /** @throws {NotFound | Timeout} */
  override get(id: string): string { // @expect TSR1304
    if (id === "slow") throw new Timeout("slow");
    return super.get(id);
  }
}
export const v: string = new Remote().get("a");
