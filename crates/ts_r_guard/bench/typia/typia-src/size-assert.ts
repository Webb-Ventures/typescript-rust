// Size: ten typia.createAssert<T>() (the counterpart of ten parse<T>).
import typia from "typia";
import type { User, Address, Money, Product, OrderItem, OrderStatus, Order, Page, ApiError, Event } from "./types";

export const validators = [
  typia.createAssert<User>(), typia.createAssert<Address>(), typia.createAssert<Money>(),
  typia.createAssert<Product>(), typia.createAssert<OrderItem>(), typia.createAssert<OrderStatus>(),
  typia.createAssert<Order>(), typia.createAssert<Page<User>>(), typia.createAssert<ApiError>(),
  typia.createAssert<Event>(),
];
