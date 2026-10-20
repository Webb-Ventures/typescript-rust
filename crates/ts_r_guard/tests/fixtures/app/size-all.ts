// Size measurement: validators for the ten API types of types.ts.
import { parse } from "@ts-r.dev/guard";
import type { User, Address, Money, Product, OrderItem, OrderStatus, Order, Page, ApiError, Event } from "./types";

export const validators = [
  parse<User>, parse<Address>, parse<Money>, parse<Product>, parse<OrderItem>,
  parse<OrderStatus>, parse<Order>, parse<Page<User>>, parse<ApiError>, parse<Event>,
];
