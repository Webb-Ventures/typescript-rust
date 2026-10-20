// undefined written into an optional property that doesn't allow it.
interface Options {
  readonly timeout?: number;
}
export const o: Options = { timeout: undefined }; // @expect TSR0402
