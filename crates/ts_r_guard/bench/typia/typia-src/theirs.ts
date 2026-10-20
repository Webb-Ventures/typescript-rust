// typia validators for the ten API types: is, assert (throws TypeGuardError on the first
// error), validate (collects every error).
import typia from "typia";
import type { User, Address, Money, Product, OrderItem, OrderStatus, Order, Page, ApiError, Event } from "./types";

export const validators = {
  User: { is: typia.createIs<User>(), parse: typia.createAssert<User>(), check: typia.createValidate<User>() },
  Address: { is: typia.createIs<Address>(), parse: typia.createAssert<Address>(), check: typia.createValidate<Address>() },
  Money: { is: typia.createIs<Money>(), parse: typia.createAssert<Money>(), check: typia.createValidate<Money>() },
  Product: { is: typia.createIs<Product>(), parse: typia.createAssert<Product>(), check: typia.createValidate<Product>() },
  OrderItem: { is: typia.createIs<OrderItem>(), parse: typia.createAssert<OrderItem>(), check: typia.createValidate<OrderItem>() },
  OrderStatus: { is: typia.createIs<OrderStatus>(), parse: typia.createAssert<OrderStatus>(), check: typia.createValidate<OrderStatus>() },
  Order: { is: typia.createIs<Order>(), parse: typia.createAssert<Order>(), check: typia.createValidate<Order>() },
  "Page<User>": { is: typia.createIs<Page<User>>(), parse: typia.createAssert<Page<User>>(), check: typia.createValidate<Page<User>>() },
  ApiError: { is: typia.createIs<ApiError>(), parse: typia.createAssert<ApiError>(), check: typia.createValidate<ApiError>() },
  Event: { is: typia.createIs<Event>(), parse: typia.createAssert<Event>(), check: typia.createValidate<Event>() },
};
