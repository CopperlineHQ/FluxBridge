// SPDX-License-Identifier: LGPL-3.0-or-later

#![cfg(feature = "hardware-tests")]

//! Explicit, ignored probes for a user-selected physical interface and disk.

use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use fluxbridge::flux::{RevolutionScan, scan_revolution};
use fluxbridge::{
    Bridge, BridgeConfig, BridgeEvent, DriveSelect, DriverKind, PortId, PortSelection, Side,
    TrackAddress, TrackCapture, WriteRequest, ports,
};

const AMIGA_MFM_MASK: u32 = 0x5555_5555;

fn hardware_config() -> BridgeConfig {
    let driver = std::env::var("FLUXBRIDGE_TEST_DRIVER")
        .expect("set FLUXBRIDGE_TEST_DRIVER to an exact driver token");
    let port =
        std::env::var("FLUXBRIDGE_TEST_PORT").expect("set FLUXBRIDGE_TEST_PORT to an exact PortId");
    let drive = match std::env::var("FLUXBRIDGE_TEST_DRIVE")
        .unwrap_or_else(|_| "pc-a".into())
        .as_str()
    {
        "pc-a" => DriveSelect::PcA,
        "pc-b" => DriveSelect::PcB,
        "shugart-0" => DriveSelect::Shugart0,
        "shugart-1" => DriveSelect::Shugart1,
        "shugart-2" => DriveSelect::Shugart2,
        "shugart-3" => DriveSelect::Shugart3,
        value => panic!("invalid FLUXBRIDGE_TEST_DRIVE {value:?}"),
    };
    BridgeConfig {
        driver: DriverKind::from_str(&driver).expect("valid FLUXBRIDGE_TEST_DRIVER"),
        port: PortSelection::Exact(PortId::from_str(&port).expect("valid FLUXBRIDGE_TEST_PORT")),
        drive,
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

fn wait_for_capture(bridge: &mut Bridge, track: TrackAddress) -> TrackCapture {
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        if let Some(capture) = bridge.read_track(track).expect("poll capture") {
            return capture;
        }
        while let Some(event) = bridge.poll_event() {
            match event {
                BridgeEvent::Disconnected(error) => {
                    panic!("hardware worker failed: {error:?}")
                }
                other => println!("hardware event while waiting for capture: {other:?}"),
            }
        }
        assert!(
            Instant::now() < deadline,
            "no track capture within seven seconds; track={track:?}; status={:?}",
            bridge.status()
        );
        thread::sleep(Duration::from_millis(5));
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
    let capture = wait_for_capture(&mut bridge, track);
    println!(
        "captured {} bits from {}: {:?}; {:?}",
        capture.bit_len(),
        bridge.selected_port(),
        capture.quality(),
        scan_revolution(capture.words(), capture.bit_len())
    );
    assert_eq!(
        scan_revolution(capture.words(), capture.bit_len()),
        RevolutionScan::CleanAmigaDos { sectors: 11 },
        "track 0 side 0 is not a clean AmigaDOS DD revolution"
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

#[inline]
fn bit_at(words: &[u16], bit_len: usize, position: usize) -> u32 {
    let position = position % bit_len;
    u32::from(words[position / 16] & (1 << (15 - position % 16)) != 0)
}

fn long_at(words: &[u16], bit_len: usize, start: usize) -> u32 {
    (0..32).fold(0, |value, offset| {
        (value << 1) | bit_at(words, bit_len, start + offset)
    })
}

#[inline]
const fn deinterleave(odd: u32, even: u32) -> u32 {
    ((odd & AMIGA_MFM_MASK) << 1) | (even & AMIGA_MFM_MASK)
}

fn decode_amigados_sector(
    capture: &TrackCapture,
    expected_track: u8,
    expected_sector: u8,
) -> Option<Vec<u8>> {
    let words = capture.words();
    let bit_len = capture.bit_len();
    let mut window = 0_u16;
    for position in 0..bit_len + 15 {
        window = (window << 1) | bit_at(words, bit_len, position) as u16;
        if position < 31 || window != 0x4489 {
            continue;
        }
        let first_sync = position + bit_len - 15;
        let second_sync = first_sync + 16;
        if long_at(words, bit_len, first_sync) >> 16 != 0x4489
            || long_at(words, bit_len, second_sync) >> 16 != 0x4489
        {
            continue;
        }

        let body = second_sync + 16;
        let info = deinterleave(
            long_at(words, bit_len, body),
            long_at(words, bit_len, body + 32),
        );
        let [format, track, sector, _to_gap] = info.to_be_bytes();
        if format != 0xff || track != expected_track || sector != expected_sector {
            continue;
        }

        let data_start = body + 448;
        let mut data = Vec::with_capacity(512);
        for index in 0..128 {
            let value = deinterleave(
                long_at(words, bit_len, data_start + index * 32),
                long_at(words, bit_len, data_start + (128 + index) * 32),
            );
            data.extend_from_slice(&value.to_be_bytes());
        }
        return Some(data);
    }
    None
}

#[test]
#[ignore = "requires explicitly selected physical hardware and an AmigaDOS disk"]
fn amigados_sample_probe() {
    let mut bridge = Bridge::open(&hardware_config()).expect("open selected bridge");
    bridge
        .set_motor(Side::Lower, true)
        .expect("start drive motor");

    let samples = [
        (
            TrackAddress {
                cylinder: 0,
                side: Side::Lower,
            },
            0,
        ),
        (
            TrackAddress {
                cylinder: 0,
                side: Side::Upper,
            },
            1,
        ),
        (
            TrackAddress {
                cylinder: 1,
                side: Side::Lower,
            },
            2,
        ),
        (
            TrackAddress {
                cylinder: 1,
                side: Side::Upper,
            },
            3,
        ),
        (
            TrackAddress {
                cylinder: 40,
                side: Side::Lower,
            },
            80,
        ),
        (
            TrackAddress {
                cylinder: 40,
                side: Side::Upper,
            },
            81,
        ),
        (
            TrackAddress {
                cylinder: 79,
                side: Side::Lower,
            },
            158,
        ),
        (
            TrackAddress {
                cylinder: 79,
                side: Side::Upper,
            },
            159,
        ),
    ];
    let mut boot_sector = None;
    let mut root_sector = None;

    for (track, amiga_track) in samples {
        bridge.seek(track).expect("seek sampled track");
        let capture = wait_for_capture(&mut bridge, track);
        let scan = scan_revolution(capture.words(), capture.bit_len());
        println!(
            "track {amiga_track:3} ({:?}): {} bits, generation {}, {:?}, {:?}",
            track,
            capture.bit_len(),
            capture.generation(),
            capture.quality(),
            scan
        );
        assert_eq!(
            scan,
            RevolutionScan::CleanAmigaDos { sectors: 11 },
            "sampled track {amiga_track} is not a clean AmigaDOS DD revolution"
        );
        if amiga_track == 0 {
            boot_sector = decode_amigados_sector(&capture, amiga_track, 0);
        } else if amiga_track == 80 {
            root_sector = decode_amigados_sector(&capture, amiga_track, 0);
        }
    }

    let boot_sector = boot_sector.expect("decode AmigaDOS boot sector");
    assert_eq!(&boot_sector[..3], b"DOS", "boot-sector signature");
    println!("boot block filesystem variant: DOS\\{}", boot_sector[3]);

    let root_sector = root_sector.expect("decode AmigaDOS root block");
    let name_len = usize::from(root_sector[432]).min(30);
    assert!(name_len > 0, "root block has an empty volume name");
    let volume_name = String::from_utf8_lossy(&root_sector[433..433 + name_len]);
    println!("AmigaDOS root block volume name: {volume_name}");
}
