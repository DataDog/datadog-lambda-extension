import handler


def test_first_order_is_stored(replay):
    with replay.invocation(0) as inv:
        result = inv.invoke(handler.handler)
    assert result["order_id"] == "o-1001"
    assert result["qty"] == 2
    assert result["stored_at"].endswith("/orders/o-1001.json")


def test_second_order_reads_back_quantity(replay):
    with replay.invocation(1) as inv:
        result = inv.invoke(handler.handler)
    assert result["qty"] == 5


def test_reorder_overwrites_quantity(replay):
    with replay.invocation(2) as inv:
        result = inv.invoke(handler.handler)
    assert (result["order_id"], result["qty"]) == ("o-1001", 3)
    assert inv.missing == []
