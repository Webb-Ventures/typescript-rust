// ts-r parse<T> validators for the ten API types: is (fast path), parse (throws ParseError),
// safeParse (collects up to 20 issues).
import { is, parse, safeParse } from "@ts-r.dev/guard";
import type { User, Address, Money, Product, OrderItem, OrderStatus, Order, Page, ApiError, Event } from "./types";

export const validators = {
  User: { is: is<User>, parse: parse<User>, check: safeParse<User> },
  Address: { is: is<Address>, parse: parse<Address>, check: safeParse<Address> },
  Money: { is: is<Money>, parse: parse<Money>, check: safeParse<Money> },
  Product: { is: is<Product>, parse: parse<Product>, check: safeParse<Product> },
  OrderItem: { is: is<OrderItem>, parse: parse<OrderItem>, check: safeParse<OrderItem> },
  OrderStatus: { is: is<OrderStatus>, parse: parse<OrderStatus>, check: safeParse<OrderStatus> },
  Order: { is: is<Order>, parse: parse<Order>, check: safeParse<Order> },
  "Page<User>": { is: is<Page<User>>, parse: parse<Page<User>>, check: safeParse<Page<User>> },
  ApiError: { is: is<ApiError>, parse: parse<ApiError>, check: safeParse<ApiError> },
  Event: { is: is<Event>, parse: parse<Event>, check: safeParse<Event> },
};
