// SPDX-License-Identifier: LGPL-3.0-or-later

#![cfg(feature = "hardware-tests")]

//! Explicit, ignored probes for a user-selected physical interface and disk.

use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use fluxbridge::{
    Bridge, BridgeConfig, BridgeEvent, DriverKind, PortId, PortSelection, Side, TrackAddress,
    WriteRequest, ports,
};

fn hardware_config() -> BridgeConfig {
    let driver = std::env::var("FLUXBRIDGE_TEST_DRIVER")
        .expect("set FLUXBRIDGE_TEST_DRIVER to an exact driver token");
    let port =
        std::env::var("FLUXBRIDGE_TEST_PORT").expect("set FLUXBRIDGE_TEST_PORT to an exact PortId");
    BridgeConfig {
        driver: DriverKind::from_str(&driver).expect("valid FLUXBRIDGE_TEST_DRIVER"),
        port: PortSelection::Exact(PortId::from_str(&port).expect("valid FLUXBRIDGE_TEST_PORT")),
        ..BridgeConfig::default()
    }
}

#[test]
#[ignore = "requires explicitly selected physical hardware"]
fn inventory() {
    for port in ports().expect("enumerate ports") {
        println!("{port:?}");
    }
}

#[test]
#[ignore = "requires explicitly selected physical hardware and a disk"]
fn status_and_track_probe() {
    let mut bridge = Bridge::open(&hardware_config()).expect("open selected bridge");
    let track = TrackAddress {
        cylinder: 0,
        side: Side::Lower,
    };
    bridge.set_motor(track.side, true).expect("start motor");
    bridge.seek(track).expect("seek cylinder zero");
    let deadline = Instant::now() + Duration::from_secs(5);
    let capture = loop {
        if let Some(capture) = bridge.read_track(track).expect("poll capture") {
            break capture;
        }
        assert!(
            Instant::now() < deadline,
            "no track capture within five seconds; status={:?}",
            bridge.status()
        );
        thread::sleep(Duration::from_millis(5));
    };
    println!(
        "captured {} bits from {}: {:?}",
        capture.bit_len(),
        bridge.selected_port(),
        capture.quality()
    );

    if std::env::var_os("FLUXBRIDGE_TEST_WRITE").is_some() {
        eprintln!("WARNING: writing the captured track back to the real disk");
        let id = bridge
            .submit_write(WriteRequest {
                track,
                bit_len: capture.bit_len(),
                words: capture.into_words(),
                start_bit: 0,
            })
            .expect("submit hardware write");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match bridge.poll_event() {
                Some(BridgeEvent::WriteCompleted { id: completed, .. }) if completed == id => break,
                Some(BridgeEvent::WriteFailed {
                    id: failed, error, ..
                }) if failed == id => panic!("hardware write failed: {error}"),
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "write did not complete within five seconds"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
