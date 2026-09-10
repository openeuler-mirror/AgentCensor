import unittest

from payment import PaymentService


class Gateway:
    def __init__(self):
        self.calls = []

    def charge(self, order_id, amount):
        self.calls.append((order_id, amount))
        return f"receipt-{len(self.calls)}"


class PaymentTests(unittest.TestCase):
    def test_retry_does_not_charge_twice(self):
        gateway = Gateway()
        service = PaymentService(gateway)
        first = service.charge("order-7", 100)
        second = service.charge("order-7", 100)
        self.assertEqual(first, second)
        self.assertEqual(gateway.calls, [("order-7", 100)])


if __name__ == "__main__":
    unittest.main()
