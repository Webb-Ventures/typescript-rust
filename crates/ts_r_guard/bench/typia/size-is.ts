// Size: ten is<T>.
import { is } from "@ts-r.dev/guard";
import type { User, Address, Money, Product, OrderItem, OrderStatus, Order, Page, ApiError, Event } from "./types";

export const validators = [
  is<User>, is<Address>, is<Money>, is<Product>, is<OrderItem>,
  is<OrderStatus>, is<Order>, is<Page<User>>, is<ApiError>, is<Event>,
];
