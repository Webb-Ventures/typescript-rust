// Ten typical API types (record 0007, size measurement).
export interface User {
  id: number;
  email: string;
  name: string;
  role: "admin" | "member" | "guest";
  createdAt: string;
  avatarUrl?: string;
}

export interface Address {
  line1: string;
  line2?: string;
  city: string;
  postcode: string;
  country: "AU" | "NZ" | "US";
}

export interface Money {
  amount: number;
  currency: "AUD" | "USD";
}

export interface Product {
  id: string;
  sku: `SKU-${number}`;
  title: string;
  price: Money;
  tags: string[];
  stock: number;
  active: boolean;
}

export interface OrderItem {
  productId: string;
  quantity: number;
  unitPrice: Money;
}

export type OrderStatus = "pending" | "paid" | "shipped" | "cancelled";

export interface Order {
  id: string;
  userId: number;
  items: OrderItem[];
  status: OrderStatus;
  shipping: Address;
  total: Money;
  notes: string | null;
}

export interface Page<T> {
  items: T[];
  next: string | null;
  total: number;
}

export interface ApiError {
  code: string;
  message: string;
  details?: Record<string, string>;
}

export type Event =
  | { type: "order.created"; order: Order }
  | { type: "user.updated"; user: User }
  | { type: "ping"; at: number };
