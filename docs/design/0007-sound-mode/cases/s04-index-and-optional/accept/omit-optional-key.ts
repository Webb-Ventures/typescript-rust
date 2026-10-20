// Leaving the key out instead of writing undefined.
interface Options {
  readonly timeout?: number;
}
function make(timeout: number | undefined): Options {
  return timeout === undefined ? {} : { timeout };
}
export const o: Options = make(undefined);
