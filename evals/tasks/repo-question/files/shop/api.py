from .payments import refund, RefundTooLarge


def post_refund(request, orders, gateway):
    order = orders[request["order_id"]]
    try:
        refund(order, request["amount"], gateway)
    except RefundTooLarge as e:
        return {"status": 422, "error": str(e)}
    return {"status": 200}
