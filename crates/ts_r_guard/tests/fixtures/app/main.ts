#!/usr/bin/env node
"use strict";
import { parse, safeParse, is, assert, parseJson, guard, env, body, query, args, expect, panic, ParseError } from "@ts-r.dev/guard";
import * as g from "@ts-r.dev/guard";
import type { User, Order, Product, Address, OrderItem, Page, Event, ApiError, Money } from "./types";
import type { Chain, Env, Search, Cli, Placed, Nest, Flag } from "./extra";
import { report } from "./report";
import { Point } from "./models";
import type { Email, Port, Positive } from "./validators";

const money: Money = { amount: 12.5, currency: "AUD" };
const address = { line1: "1 George St", city: "Sydney", postcode: "2000", country: "AU" };
const user = { id: 1, email: "a@b.c", name: "Ann", role: "admin", createdAt: "2026-10-15" };
const item = { productId: "p1", quantity: 2, unitPrice: money };
const order = {
  id: "o1", userId: 1, items: [item], status: "paid", shipping: address, total: money, notes: null,
};

report("parse User valid", () => parse<User>(user));
report("parse User invalid", () => parse<User>({ ...user, id: "1", role: "owner", email: 5 }));
report("parse User missing", () => parse<User>({ id: 1 }));
report("parse User optional undefined", () => parse<User>({ ...user, avatarUrl: undefined }));
report("parse User not object", () => parse<User>([user]));
report("parse User same object", () => parse<User>(user) === user);
report("safeParse Order valid", () => safeParse<Order>(order).ok);
report("safeParse Order invalid", () => {
  const r = safeParse<Order>({ ...order, items: [{ ...item, quantity: "2" }], status: "lost" });
  return r.ok ? "ok" : { type: r.error.type, issues: r.error.issues, isParseError: r.error instanceof ParseError };
});
report("is Product", () => [
  is<Product>({ id: "p1", sku: "SKU-12", title: "T", price: money, tags: [], stock: 1, active: true }),
  is<Product>({ id: "p1", sku: "SKU-x", title: "T", price: money, tags: [], stock: 1, active: true }),
]);
report("assert Address", () => {
  const a: unknown = { ...address, country: "UK" };
  assert<Address>(a);
  return a.city;
});
report("parseJson Page<User>", () => parseJson<Page<User>>(JSON.stringify({ items: [user], next: null, total: 1 })).total);
report("parseJson Page<User> invalid", () => parseJson<Page<User>>(`{"items":[{"id":1}],"next":3,"total":1}`));
report("parseJson syntax", () => parseJson<Page<User>>("{"));
const events = guard<Event>();
report("guard Event", () => [
  events.is({ type: "ping", at: 1 }),
  events.is({ type: "user.updated", user }),
  events.is({ type: "nope" }),
]);
report("guard Event invalid", () => events.parse({ type: "order.created", order: { ...order, total: null } }));
report("map parse<OrderItem>", () => [item, item].map(parse<OrderItem>).length);
report("map parse<OrderItem> invalid", () => [item, { productId: 1 }].map(parse<OrderItem>));
report("namespace parse", () => g.parse<Money>({ amount: "1", currency: "EUR" }));
report("namespace ParseError", () => new g.ParseError("x", []) instanceof Error);
report("filter is<Money>", () => [money, {}, { amount: 1, currency: "USD" }].filter(is<Money>).length);
report("env", () => env<Env>());
report("env invalid", () => env<{ GUARD_PORT: number; GUARD_MISSING: string }>());
report("body", () => body<Money>({ body: { amount: 1, currency: "AUD" } }));
report("body invalid", () => body<Money>({ body: "{}" }));
report("query parsed", () => query<Search>({ query: { q: "shoes", page: "2", tags: ["a", "b"], sort: "asc" } }));
report("query url", () => query<Search>({ url: "/s?q=hat&tags=x&sort=desc" }));
report("query invalid", () => query<Search>(new URLSearchParams("q=hat&page=two&sort=up")));
report("args", () =>
  args<Cli>(["build", "--port", "8080", "--verbose", "--dry-run", "--include=a", "--include", "b", "--", "--x"]),
);
report("args invalid", () => args<Cli>(["--port", "eighty", "--no-verbose"]));
report("expect", () => expect([1, 2].find((x) => x > 1), "no item"));
report("expect fails", () => expect([1, 2].find((x) => x > 5), "no item above 5"));
report("panic", () => panic("boom"));
report("deep", () => {
  let v: unknown = null;
  for (let i = 0; i < 600; i++) v = { next: v };
  const r = safeParse<Chain>(v);
  return r.ok ? "accepted" : [r.error.type, r.error.issues.length, r.error.issues[0]?.path.length, r.error.issues[0]?.received];
});
report("chain ok", () => is<Chain>({ next: { next: null } }));
report("class Point", () => parse<Point>(new Point(1, 2)).x);
report("class Point invalid", () => parse<Point>({ x: 1, y: 2 }));
report("refined Email", () => parse<Email>("ann@example.com"));
report("refined Email invalid", () => parse<Email>("not an email"));
report("refined Email not a string", () => parse<Email>(5));
report("refined query", () => query<{ port: Port }>({ query: { port: "3000" } }));
report("refined query invalid", () => query<{ port: Port }>({ query: { port: "70000" } }));
report("refined alias", () => parse<Positive>({ x: 1 }));
report("refined alias invalid", () => parse<Positive>({ x: -1 }));
report("placed", () => is<Placed>({ at: new Point(0, 0), owner: "a@b.co" }));
report("placed invalid", () => parse<Placed>({ at: { x: 0, y: 0 }, owner: "nope" }));
report("parse inside a getter", () =>
  parse<{ a: number; b: string }>({
    a: "bad",
    get b() {
      try {
        parse<number>("inner");
      } catch {}
      return 5;
    },
  }),
);
report("query recursive array", () => {
  try {
    return query<{ X: Nest }>(new URLSearchParams("X=x")) && "accepted";
  } catch (e) {
    return e instanceof ParseError ? [e.type, e.issues.length, e.issues[0]?.path.length, e.issues[0]?.received] : String(e);
  }
});
report("args boolean alias", () => args<{ verbose: Flag; _: string[] }>(["--verbose", "file.ts"]));

// Dead code: its validator must not reach a bundle.
function unusedApiError(x: unknown): ApiError {
  return parse<ApiError>(x);
}
