//! Week 17 (integration): the throttles and the week-15 limits as the
//! gateway's pre-trade gate, over TCP.

use gateway::client::Client;
use gateway::{Gateway, GatewayConfig, Hooks, NoStage, RiskGate};
use risk::{BucketConfig, Limits};
use std::time::Duration;
use types::{Event, NewOrder, OrderId, Qty, RejectReason, RiskReason, Side};

#[test]
fn w17_risk_gate_in_the_gateway() {
    // Buckets that never refill during the test: one token per hour.
    let bucket = |burst| BucketConfig {
        burst,
        ns_per_token: 3_600_000_000_000,
    };
    let limits = Limits {
        max_order_qty: Qty(100),
        max_position: Qty(45),
        ..Limits::UNLIMITED
    };
    let gate = RiskGate::new(limits, bucket(8), bucket(10));
    let kill = gate.risk().global_kill_switch();
    let gw = Gateway::start_with(
        GatewayConfig::default(),
        Hooks {
            pretrade: Box::new(gate),
            stage: Box::new(NoStage),
        },
    )
    .unwrap();
    let login = |account| {
        let mut c = Client::connect(gw.local_addr()).unwrap();
        c.logon(account, 0).unwrap();
        c
    };
    let buy = |id, qty| NewOrder::limit(id, Side::Buy, 90, qty);
    let risk = |id, r| Event::Rejected {
        id: OrderId(id),
        reason: RejectReason::Risk(r),
    };
    let ok = |c: &mut Client, id| {
        assert_eq!(
            c.recv_events(2).unwrap()[0],
            Event::Accepted { id: OrderId(id) }
        );
    };

    let mut a = login(1);
    a.send_order(&buy(1, 200)).unwrap();
    assert_eq!(a.recv_event().unwrap(), risk(1, RiskReason::MaxOrderQty));
    for id in 2..=5 {
        a.send_order(&buy(id, 10)).unwrap();
        ok(&mut a, id);
    }
    // 40 open + 10 > 45: the projected position counts open orders.
    a.send_order(&buy(6, 10)).unwrap();
    assert_eq!(a.recv_event().unwrap(), risk(6, RiskReason::PositionLimit));
    // A cancel (never throttled) releases the open quantity.
    a.cancel(2).unwrap();
    assert_eq!(
        a.recv_event().unwrap(),
        Event::Cancelled {
            id: OrderId(2),
            remaining: Qty(10)
        }
    );
    a.send_order(&buy(7, 10)).unwrap();
    ok(&mut a, 7);

    // A second session of the same account shares the account bucket
    // (10 tokens; A used 7).
    let mut b = login(1);
    for id in 1..=3 {
        b.send_order(&buy(id, 1)).unwrap();
        ok(&mut b, id);
    }
    b.send_order(&buy(4, 1)).unwrap();
    assert_eq!(b.recv_event().unwrap(), risk(4, RiskReason::RateLimit));

    // Another account is unaffected, until the global kill switch.
    let mut c = login(2);
    c.send_order(&buy(1, 1)).unwrap();
    ok(&mut c, 1);
    kill.engage();
    c.send_order(&buy(2, 1)).unwrap();
    assert_eq!(c.recv_event().unwrap(), risk(2, RiskReason::KillSwitch));
    kill.disengage();
    c.send_order(&buy(3, 1)).unwrap();
    ok(&mut c, 3);

    // Only passing orders and the cancel reached the engine.
    assert_eq!(gw.stats().sequenced, 4 + 1 + 1 + 3 + 2);
    assert!(gw.shutdown(Duration::from_secs(10)).is_clean());
}
