// An async function's rejections are its throws set.
class NotFound extends Error {
  readonly code = "NOT_FOUND";
}
class Timeout extends Error {
  readonly code = "TIMEOUT";
}
/** @throws {Timeout} */
export async function wait(ms: number): Promise<void> {
  if (ms > 1000) throw new Timeout("too long");
}
