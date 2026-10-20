import logo from "./logo.svg";
import { chunk, Counter, first, mapAll } from "tiny-lib";

export const banner = `${APP_NAME} ${APP_VERSION} ${logo}`;

export function pages<T>(items: T[]): T[][] {
  return chunk(items, { size: 10 });
}

export function lastPage<T>(items: T[]): T[] | undefined {
  return pages(items).lastItem();
}

export const counter = new Counter(1);

export function labels(n: number[]): string[] {
  return mapAll(n, (v, i) => `${i}:${v}`);
}

export function firstOr<T>(items: T[], d: T): T {
  return first(items) ?? d;
}

export function settings(): Settings {
  return { theme: "dark", fontSize: 12 };
}
