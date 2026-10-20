// A public method of an exported class counts as exported (TSR1302 only with exports-declare-throws).
class NotFound extends Error {
  readonly code = "NOT_FOUND";
}
class Timeout extends Error {
  readonly code = "TIMEOUT";
}
export class Repo {
  get(id: string): string { // @opt-in TSR1302
    if (id === "") throw new NotFound("missing");
    return id;
  }
}
