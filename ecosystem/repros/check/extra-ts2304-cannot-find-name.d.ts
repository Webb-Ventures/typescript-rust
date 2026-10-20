// Cause:    extra TS2304: Cannot find name '…'.
// Found in: @types/node@26.6.5 (Disposable, AsyncDisposable, Float16Array: index.d.ts has
//           `/// <reference lib="esnext.disposable" />` and `esnext.float16`)
// Owner:    ts_r_checker (lib references from declaration files in node_modules)
// Expected: tsc 7 (target es2022, default libs) adds the lib named by a `<reference lib>`
//           directive in any program file, so `Disposable` resolves.
// Actual:   ts-r-check reports TS2304 "Cannot find name 'Disposable'."
// See also: extra-ts2304-merged-ambient-module.d.ts (WithImplicitCoercion in @types/node)
/// <reference lib="esnext.disposable" />
export declare function open(): Disposable;
