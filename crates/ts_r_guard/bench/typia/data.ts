// One valid and one invalid value per type (the invalid ones have 2-3 wrong fields).
const user = {
  id: 1,
  email: "ann@example.com",
  name: "Ann",
  role: "admin",
  createdAt: "2026-01-02T03:04:05Z",
  avatarUrl: "https://example.com/a.png",
};
const address = { line1: "1 Main St", city: "Sydney", postcode: "2000", country: "AU" };
const money = { amount: 12.5, currency: "AUD" };
const product = {
  id: "p1",
  sku: "SKU-123",
  title: "Hat",
  price: money,
  tags: ["summer", "sale", "new"],
  stock: 4,
  active: true,
};
const item = { productId: "p1", quantity: 2, unitPrice: money };
const order = {
  id: "o1",
  userId: 1,
  items: [item, item, item],
  status: "paid",
  shipping: address,
  total: { amount: 75, currency: "AUD" },
  notes: null,
};

export const data: Record<string, { valid: unknown; invalid: unknown }> = {
  User: { valid: user, invalid: { ...user, id: "1", role: "owner" } },
  Address: { valid: address, invalid: { ...address, city: null, country: "UK" } },
  Money: { valid: money, invalid: { amount: "12.5", currency: "EUR" } },
  Product: {
    valid: product,
    invalid: { ...product, sku: "ABC-1", tags: ["a", 2], price: { amount: 1 } },
  },
  OrderItem: { valid: item, invalid: { ...item, quantity: "2", unitPrice: null } },
  OrderStatus: { valid: "shipped", invalid: "lost" },
  Order: {
    valid: order,
    invalid: { ...order, items: [item, { ...item, quantity: "x" }], status: "lost" },
  },
  "Page<User>": {
    valid: { items: [user, user, user], next: "c2", total: 3 },
    invalid: { items: [user, { ...user, email: 5 }], next: 1, total: 2 },
  },
  ApiError: {
    valid: { code: "E1", message: "bad", details: { field: "email", reason: "taken" } },
    invalid: { code: 1, message: "bad", details: { field: 2 } },
  },
  Event: {
    valid: { type: "order.created", order },
    invalid: { type: "user.updated", user: { ...user, name: 3 } },
  },
};
