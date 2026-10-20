// Type declarations of @ts-r.dev/guard as the cases see them (0007-sound-mode.md, "API").
declare module "@ts-r.dev/guard" {
  export interface ParseIssue {
    readonly path: readonly (string | number)[];
    readonly expected: string;
    readonly received: string;
  }
  export class ParseError extends Error {
    readonly name: "ParseError";
    readonly type: string;
    readonly issues: readonly ParseIssue[];
  }
  export type Result<T> =
    | { readonly ok: true; readonly value: T }
    | { readonly ok: false; readonly error: ParseError };
  export interface Guard<T> {
    /** @throws {ParseError} */
    parse(value: unknown): T;
    is(value: unknown): value is T;
  }
  /** @throws {ParseError} */
  export function parse<T = unknown>(value: unknown): T;
  export function safeParse<T = unknown>(value: unknown): Result<T>;
  export function is<T = unknown>(value: unknown): value is T;
  /** @throws {ParseError} */
  export function assert<T = unknown>(value: unknown): asserts value is T;
  /** @throws {ParseError} */
  export function parseJson<T = unknown>(text: string): T;
  /** @throws {ParseError} */
  export function fetchJson<T = unknown>(input: string | URL | Request, init?: RequestInit): Promise<T>;
  /** @throws {ParseError} */
  export function env<E = unknown>(): E;
  export function guard<T = unknown>(): Guard<T>;
  /** Unchecked for S-13: throws ExpectError when the value is null or undefined. */
  export function expect<T>(value: T, message: string): NonNullable<T>;
  /** Unchecked for S-13: always throws PanicError. */
  export function panic(message: string): never;
}
