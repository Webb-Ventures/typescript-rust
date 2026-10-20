#!/usr/bin/env node
'use strict';
// A tour of TypeScript syntax.
import type { Config } from './config';
import fs, { readFile as rf, type Stats } from 'node:fs';
import * as path from 'node:path';
import data from './data.json' with { type: 'json' };

export const enum Color { Red, Green = 'green', Blue = 1 << 2 }

declare global {
  interface Window { app: App }
}

export namespace Util.Inner {
  export function id<T>(x: T): T { return x; }
}

/** A generic interface. */
export interface Repo<T extends { id: string }, K = keyof T> extends Iterable<T> {
  readonly items: ReadonlyArray<T>;
  get(id: string): T | undefined;
  find?<U extends T>(pred: (x: T) => x is U): U[];
  [Symbol.iterator](): Iterator<T>;
  new (seed: T[]): Repo<T, K>;
  (callable: true): void;
}

type Mapped<T> = { readonly [P in keyof T as `get${Capitalize<P & string>}`]-?: () => T[P] };
type Cond<T> = T extends Promise<infer U> ? Cond<U> : T extends readonly (infer E)[] ? E : never;
type Tuple = [name: string, age?: number, ...tags: string[]];

@sealed
export abstract class Base<in out T> implements Repo<any> {
  static #instances = 0;
  #secret?: string;
  declare readonly kind: string;
  protected abstract load(): Promise<T>;
  accessor count = 0;
  [key: string]: unknown;

  constructor(private readonly name: string, @inject('x') public x?: number) {
    super();
    Base.#instances++;
  }

  get items() { return [] as const; }
  set items(_v) {}
  static { Base.#instances = 0; }
  async *stream(): AsyncGenerator<T, void, unknown> {
    for await (const chunk of this) yield chunk as T;
  }
}

export default async function main(args: string[] = process.argv.slice(2)): Promise<number> {
  const { a = 1, b: { c } = { c: 2 }, ...rest } = JSON.parse(args[0] ?? '{}');
  const [first, , third = 3] = args;
  let total = 0n;
  label: for (let i = 0; i < args.length; i++) {
    if (i % 2 === 0) continue label;
    else if (i > 10) break label;
    total += BigInt(i) ** 2n;
  }
  const fn = async <T,>(x: T): Promise<T> => x;
  const check = (v: unknown): v is string => typeof v === 'string';
  const re = /ab+c\/[a-z]/giu;
  const tpl = `sum: ${total} ${`nested ${a}`}`;
  const opt = rest?.deep?.[0]?.call?.(null)!;
  const sat = { x: 1 } satisfies Record<string, number>;
  using res = open();
  try {
    await fs.promises.readFile(path.join(__dirname, 'x'));
  } catch (err: unknown) {
    throw new Error(String(err), { cause: err });
  } finally {
    res[Symbol.dispose]?.();
  }
  switch (first) {
    case 'a':
    case 'b': {
      return 1;
    }
    default:
      return c + third + (fn<number>(1) as unknown as number) + (check(re) ? 1 : 0) + tpl.length + (opt ? 1 : 0) + sat.x;
  }
}

export { main as run };
export * as utils from './utils';
export type { Config as Cfg };
