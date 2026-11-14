"""Payments: captures, refunds and payouts."""


class RefundTooLarge(Exception):
    """Raised when a refund would exceed what was captured."""


class Declined(Exception):
    pass


def capture(order, amount):
    if amount <= 0:
        raise ValueError("amount must be positive")
    order.captured += amount
    return order.captured


def can_refund(order, amount):
    """Decide whether `amount` may be refunded for `order`."""
    already = sum(r.amount for r in order.refunds)
    if amount + already > order.captured:
        raise RefundTooLarge(f"refund {amount} exceeds captured {order.captured - already}")
    return True


def refund(order, amount, gateway):
    can_refund(order, amount)
    gateway.send_refund(order.id, amount)
    order.refunds.append(gateway.last_refund)
