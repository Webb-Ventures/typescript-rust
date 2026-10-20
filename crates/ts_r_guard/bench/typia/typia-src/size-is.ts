// Size: ten typia.createIs<T>().
import typia from "typia";
import type { User, Address, Money, Product, OrderItem, OrderStatus, Order, Page, ApiError, Event } from "./types";

export const validators = [
  typia.createIs<User>(), typia.createIs<Address>(), typia.createIs<Money>(),
  typia.createIs<Product>(), typia.createIs<OrderItem>(), typia.createIs<OrderStatus>(),
  typia.createIs<Order>(), typia.createIs<Page<User>>(), typia.createIs<ApiError>(),
  typia.createIs<Event>(),
];
