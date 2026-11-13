import handler


def test_widget_is_returned(replay):
    with replay.invocation(0) as inv:
        result = inv.invoke(handler.handler)
    assert result["item"] == {"name": "widget", "price": 3}
    assert result["caller_ip_octets"] == 4


def test_gadget_price(replay):
    with replay.invocation(1) as inv:
        result = inv.invoke(handler.handler)
    assert result["item"]["price"] == 7


def test_new_item(replay):
    # Not in the original recording: record it with LAMBDA_REPLAY_UPDATE=1.
    with replay.invocation("item-3", event={"id": "3"}) as inv:
        result = inv.invoke(handler.handler)
    assert result["item"]["name"] == "doohickey"
