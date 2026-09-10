class PaymentService:
    def __init__(self, gateway):
        self.gateway = gateway
        self.processed = set()

    def charge(self, order_id, amount):
        # Intentional demo bug: a retry charges before checking idempotency.
        receipt = self.gateway.charge(order_id, amount)
        if order_id in self.processed:
            return receipt
        self.processed.add(order_id)
        return receipt
