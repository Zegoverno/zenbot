"""Payouts to sellers."""
from .payments import Declined


def payout_allowed(seller, amount):
    if seller.frozen:
        raise Declined("seller account is frozen")
    return amount <= seller.balance


def pay(seller, amount, bank):
    if not payout_allowed(seller, amount):
        return False
    bank.transfer(seller.iban, amount)
    seller.balance -= amount
    return True
