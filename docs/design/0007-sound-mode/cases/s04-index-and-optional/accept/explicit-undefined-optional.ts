// An optional property that also allows undefined explicitly.
interface Options {
  readonly timeout?: number | undefined;
}
const o: Options = { timeout: undefined };
export const t: number = o.timeout ?? 1000;
