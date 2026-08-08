//! ITL NV9 banknote-validator driver using plain (non-eSSP) Smiley Secure Protocol.
// Under the `simulate` dev feature the real-validator path is cfg-swapped out for the
// simulator, so its helpers read as dead code — silence that in simulate builds only.
#![cfg_attr(feature = "simulate", allow(dead_code))]

use crate::models::{KioskError, KioskResult, PaymentEvent};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const SSP_STX: u8 = 0x7f;
const SSP_ADDRESS: u8 = 0x00;
const SSP_SEQUENCE_BIT: u8 = 0x80;

const CMD_SET_INHIBITS: u8 = 0x02;
const CMD_SETUP_REQUEST: u8 = 0x05;
#[allow(dead_code)] // kept for reference; not sent (see initialize_validator)
const CMD_HOST_PROTOCOL: u8 = 0x06;
const CMD_POLL: u8 = 0x07;
const CMD_REJECT: u8 = 0x08;
const CMD_DISABLE: u8 = 0x09;
const CMD_ENABLE: u8 = 0x0a;
const CMD_GET_SERIAL: u8 = 0x0c;
const CMD_SYNC: u8 = 0x11;

const RESPONSE_OK: u8 = 0xf0;
const RESPONSE_UNKNOWN_COMMAND: u8 = 0xf2;

#[allow(dead_code)] // kept for reference; not negotiated (see initialize_validator)
const SSP_PROTOCOL_VERSION: u8 = 6;
const SERIAL_TIMEOUT: Duration = Duration::from_millis(350);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const ESCROW_WAIT_SLICE: Duration = Duration::from_millis(100);
const COMMAND_RETRIES: usize = 2;
const MAX_LEADING_NOISE_BYTES: usize = 64;
const FRAME_READ_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Nv9Config {
    pub port: String,
    pub baud: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscrowDecision {
    Accept,
    Reject,
}

struct SspTransport {
    port: Box<dyn SerialPort>,
    next_sequence: u8,
}

impl SspTransport {
    fn new(port: Box<dyn SerialPort>) -> Self {
        Self {
            port,
            next_sequence: SSP_ADDRESS,
        }
    }

    /// SYNC itself is sent with sequence zero. A successful SYNC resets the
    /// slave so the following new command must use the set sequence bit.
    fn sync(&mut self) -> KioskResult<()> {
        self.next_sequence = SSP_ADDRESS;
        let response = self.exchange(SSP_ADDRESS, CMD_SYNC, &[])?;
        match response.first().copied() {
            Some(RESPONSE_OK) => {
                // SYNC resets the sequence chain; the first command AFTER sync goes out
                // with sequence bit 0 (verified against the working gmarull/nv9biller
                // NV9USB driver, which resets _sequence to 0 post-sync). It then toggles
                // to 0x80 on the following command as usual.
                self.next_sequence = SSP_ADDRESS;
                Ok(())
            }
            Some(code) => Err(hardware(format!(
                "NV9 SYNC returned SSP response 0x{code:02X}"
            ))),
            None => Err(hardware("NV9 SYNC returned an empty response")),
        }
    }

    fn command(&mut self, command: u8, params: &[u8]) -> KioskResult<Vec<u8>> {
        let sequence = self.next_sequence;
        // SSP uses bit 7 of the SEQ/slave byte, not bit 3 despite some old
        // summaries calling it the "sequence bit" without identifying the bit.
        self.next_sequence ^= SSP_SEQUENCE_BIT;
        self.exchange(sequence, command, params)
    }

    fn exchange(&mut self, sequence: u8, command: u8, params: &[u8]) -> KioskResult<Vec<u8>> {
        let frame = encode_packet(sequence, command, params)?;
        let mut last_error = String::new();

        // A retry retransmits the identical packet and therefore keeps the same
        // sequence bit. Only a genuinely new command toggles the bit.
        for attempt in 0..=COMMAND_RETRIES {
            let attempt_result = (|| -> KioskResult<Vec<u8>> {
                self.port
                    .write_all(&frame)
                    .map_err(|error| hardware(format!("NV9 serial write failed: {error}")))?;
                self.port
                    .flush()
                    .map_err(|error| hardware(format!("NV9 serial flush failed: {error}")))?;

                let (response_sequence, data) = read_packet(&mut *self.port)?;
                if response_sequence != sequence {
                    return Err(hardware(format!(
                        "NV9 SSP sequence mismatch: sent 0x{sequence:02X}, received 0x{response_sequence:02X}"
                    )));
                }
                Ok(data)
            })();

            match attempt_result {
                Ok(data) => return Ok(data),
                Err(error) => {
                    last_error = error.to_string();
                    if attempt < COMMAND_RETRIES {
                        // Discard a partial/corrupt response before retransmitting.
                        let _ = self.port.clear(ClearBuffer::Input);
                        std::thread::sleep(Duration::from_millis(30));
                    }
                }
            }
        }

        Err(hardware(format!(
            "NV9 SSP command 0x{command:02X} failed after {} attempts: {last_error}",
            COMMAND_RETRIES + 1
        )))
    }
}

/// CRC-16 used by SSP: seed 0xFFFF, polynomial 0x8005, MSB first.
fn crc16_ssp(bytes: &[u8]) -> u16 {
    let mut crc = 0xffff_u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn encode_packet(sequence: u8, command: u8, params: &[u8]) -> KioskResult<Vec<u8>> {
    let data_len = 1_usize
        .checked_add(params.len())
        .ok_or_else(|| hardware("NV9 SSP command length overflow"))?;
    let length = u8::try_from(data_len)
        .map_err(|_| hardware("NV9 SSP command data exceeds 255 bytes"))?;

    // CRC covers SEQ, LENGTH and DATA. STX and the CRC bytes are excluded.
    let mut body = Vec::with_capacity(data_len + 4);
    body.push(sequence);
    body.push(length);
    body.push(command);
    body.extend_from_slice(params);
    let [crc_low, crc_high] = crc16_ssp(&body).to_le_bytes();
    body.push(crc_low); // CRCL
    body.push(crc_high); // CRCH

    // TODO(essp): encrypt/authenticate the unstuffed packet body at this layer
    // before wire framing when the kiosk moves from plain SSP to encrypted eSSP.
    let mut wire = Vec::with_capacity(body.len() + 1);
    wire.push(SSP_STX); // The leading STX is the only 0x7F that is never stuffed.
    for byte in body {
        wire.push(byte);
        if byte == SSP_STX {
            wire.push(byte); // Every 0x7F in SEQ/LENGTH/DATA/CRC is doubled.
        }
    }
    Ok(wire)
}

fn read_wire_byte(port: &mut dyn SerialPort, deadline: Instant) -> KioskResult<u8> {
    if Instant::now() >= deadline {
        return Err(hardware("NV9 SSP frame read timed out"));
    }
    let mut byte = [0_u8; 1];
    port.read_exact(&mut byte)
        .map_err(|error| hardware(format!("NV9 serial read failed: {error}")))?;
    if Instant::now() >= deadline {
        return Err(hardware("NV9 SSP frame read timed out"));
    }
    Ok(byte[0])
}

fn read_unstuffed_byte(port: &mut dyn SerialPort, deadline: Instant) -> KioskResult<u8> {
    let byte = read_wire_byte(port, deadline)?;
    if byte != SSP_STX {
        return Ok(byte);
    }

    let escaped = read_wire_byte(port, deadline)?;
    if escaped == SSP_STX {
        Ok(SSP_STX)
    } else {
        Err(hardware(format!(
            "malformed NV9 SSP byte stuffing: 0x7F followed by 0x{escaped:02X}"
        )))
    }
}

fn read_packet(port: &mut dyn SerialPort) -> KioskResult<(u8, Vec<u8>)> {
    let deadline = Instant::now() + FRAME_READ_TIMEOUT;
    // Ignore a small amount of stale line noise, but remain bounded so a bad
    // stream cannot keep a money-handling command blocked indefinitely.
    let mut saw_stx = false;
    for _ in 0..MAX_LEADING_NOISE_BYTES {
        if read_wire_byte(port, deadline)? == SSP_STX {
            saw_stx = true;
            break;
        }
    }
    if !saw_stx {
        return Err(hardware("NV9 SSP response did not contain STX"));
    }

    let sequence = read_unstuffed_byte(port, deadline)?;
    let length = read_unstuffed_byte(port, deadline)?;
    let mut data = Vec::with_capacity(usize::from(length));
    for _ in 0..length {
        data.push(read_unstuffed_byte(port, deadline)?);
    }
    let crc_low = read_unstuffed_byte(port, deadline)?;
    let crc_high = read_unstuffed_byte(port, deadline)?;

    let mut protected = Vec::with_capacity(data.len() + 2);
    protected.push(sequence);
    protected.push(length);
    protected.extend_from_slice(&data);
    let expected = crc16_ssp(&protected);
    let received = u16::from_le_bytes([crc_low, crc_high]);
    if received != expected {
        return Err(hardware(format!(
            "NV9 SSP CRC mismatch: expected 0x{expected:04X}, received 0x{received:04X}"
        )));
    }

    Ok((sequence, data))
}

#[derive(Debug)]
struct SetupData {
    channel_values: Vec<i64>,
}

fn parse_setup_response(response: &[u8]) -> KioskResult<SetupData> {
    match response.first().copied() {
        Some(RESPONSE_OK) => {}
        Some(code) => {
            return Err(hardware(format!(
                "NV9 Setup Request returned SSP response 0x{code:02X}"
            )))
        }
        None => return Err(hardware("NV9 Setup Request returned an empty response")),
    }

    // Canonical SSP layout: F0 | unit(1) | firmware(4) | country(3) |
    // multiplier(3, BE) | channel count(1) | values(N) | security(N) |
    // real multiplier(3, BE) | protocol version(1).
    if response.len() < 13 {
        return Err(hardware(
            "malformed NV9 Setup Request response (fixed header is incomplete)",
        ));
    }
    let channel_count = usize::from(response[12]);
    if channel_count == 0 || channel_count > 16 {
        return Err(hardware(format!(
            "malformed NV9 Setup Request response (invalid channel count {channel_count})"
        )));
    }

    let values_start = 13;
    let security_start = values_start + channel_count;
    let real_multiplier_start = security_start + channel_count;
    let required_len = real_multiplier_start + 4;
    if response.len() < required_len {
        return Err(hardware(format!(
            "malformed NV9 Setup Request response (expected at least {required_len} bytes)"
        )));
    }

    // The REAL per-channel denominations live in an expanded block that follows
    // [real_multiplier(3) | protocol(1)]:
    //   [3*n per-channel country codes] then [4-byte little-endian value per channel].
    // On a Serbian-dinar unit these are 10/20/50/100/200/500/1000/2000/5000. The base
    // channel_values are only indices (1..n) scaled by a coarse multiplier (which read a
    // 1000 note as 700 = 7*100). Whenever the expanded block is present (response long
    // enough) we MUST use it — do NOT gate on the protocol-version byte, since without
    // negotiating protocol 6 the unit can report a lower version yet still append the block.
    let expanded_start = real_multiplier_start + 4 + 3 * channel_count;
    if response.len() >= expanded_start + 4 * channel_count {
        let mut channel_values = Vec::with_capacity(channel_count);
        for i in 0..channel_count {
            let off = expanded_start + 4 * i;
            let value = u32::from_le_bytes([
                response[off],
                response[off + 1],
                response[off + 2],
                response[off + 3],
            ]);
            channel_values.push(i64::from(value));
        }
        return Ok(SetupData { channel_values });
    }

    // Legacy path (protocol < 6): value = channel byte * multiplier.
    let value_multiplier = u24_be(&response[9..12]);
    let real_value_multiplier = u24_be(&response[real_multiplier_start..real_multiplier_start + 3]);
    let multiplier = if value_multiplier > 0 {
        value_multiplier
    } else if real_value_multiplier > 0 {
        real_value_multiplier
    } else {
        1
    };

    let mut channel_values = Vec::with_capacity(channel_count);
    for raw in &response[values_start..security_start] {
        let value = i64::from(*raw)
            .checked_mul(multiplier)
            .ok_or_else(|| hardware("NV9 channel denomination overflow"))?;
        channel_values.push(value);
    }
    Ok(SetupData { channel_values })
}

fn u24_be(bytes: &[u8]) -> i64 {
    (i64::from(bytes[0]) << 16) | (i64::from(bytes[1]) << 8) | i64::from(bytes[2])
}

fn response_description(code: u8) -> &'static str {
    match code {
        RESPONSE_UNKNOWN_COMMAND => "unknown command",
        0xf5 => "parameter out of range",
        0xf6 => "command cannot be processed",
        0xf8 => "software error",
        _ => "unrecognized negative response",
    }
}

fn acknowledge_or_notice(
    response: &[u8],
    operation: &str,
    events: &Sender<PaymentEvent>,
) -> KioskResult<bool> {
    let code = response
        .first()
        .copied()
        .ok_or_else(|| hardware(format!("NV9 {operation} returned an empty response")))?;
    if code == RESPONSE_OK {
        return Ok(true);
    }

    emit(
        events,
        PaymentEvent::Notice {
            message: format!(
                "NV9 {operation}: SSP response 0x{code:02X} ({})",
                response_description(code)
            ),
        },
    )?;
    Ok(false)
}

fn initialize_validator(
    transport: &mut SspTransport,
    events: &Sender<PaymentEvent>,
) -> KioskResult<SetupData> {
    transport.sync()?;

    // NOTE: we deliberately do NOT send Host Protocol Version (0x06). The working
    // gmarull/nv9biller NV9USB driver reads correct denominations WITHOUT negotiating a
    // protocol version, and requesting protocol 6 changes how note values are reported,
    // which broke the channel*multiplier parse on real Serbian-dinar units. Stay on the
    // unit's default protocol so Setup Request values are the classic channel*multiplier.

    // 0x05 returns identity fields plus channel denominations.
    let setup_response = transport.command(CMD_SETUP_REQUEST, &[])?;
    let setup = parse_setup_response(&setup_response)?;

    // 0x02 carries low/high inhibit masks. Set bit means enabled (not
    // inhibited): low byte channels 1-8, high byte channels 9-16.
    let mut low_mask = 0_u8;
    let mut high_mask = 0_u8;
    for (index, value) in setup.channel_values.iter().enumerate() {
        if *value <= 0 {
            continue;
        }
        if index < 8 {
            low_mask |= 1 << index;
        } else if index < 16 {
            high_mask |= 1 << (index - 8);
        }
    }
    let inhibit_response = transport.command(CMD_SET_INHIBITS, &[low_mask, high_mask])?;
    if !acknowledge_or_notice(&inhibit_response, "Set Channel Inhibits", events)? {
        return Err(hardware("NV9 Set Channel Inhibits was rejected"));
    }

    // 0x0A enables note acceptance.
    let enable_response = transport.command(CMD_ENABLE, &[])?;
    if !acknowledge_or_notice(&enable_response, "Enable", events)? {
        return Err(hardware("NV9 Enable was rejected"));
    }
    Ok(setup)
}

struct ValidatorState {
    channel_values: Vec<i64>,
    escrow_value: Option<i64>,
    total_inserted_rsd: i64,
}

enum PollDirective {
    Continue,
    Reinitialize,
    Stop,
}

enum DecisionWait {
    Accept,
    Reject,
    Stop,
}

fn wait_for_decision(
    decisions: &Receiver<EscrowDecision>,
    stop: &AtomicBool,
) -> KioskResult<DecisionWait> {
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(DecisionWait::Stop);
        }
        match decisions.recv_timeout(ESCROW_WAIT_SLICE) {
            Ok(EscrowDecision::Accept) => return Ok(DecisionWait::Accept),
            Ok(EscrowDecision::Reject) => return Ok(DecisionWait::Reject),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(hardware(
                    "NV9 escrow decision channel disconnected while a note was held",
                ))
            }
        }
    }
}

fn poll_event_payload_len(code: u8) -> usize {
    match code {
        // READ, CREDIT, front/cashbox clears and FRAUD ATTEMPT carry a channel.
        0xef | 0xee | 0xe1 | 0xe2 | 0xe6 => 1,
        // All other event codes handled below carry no payload bytes.
        0x00 | 0xf1 | 0xed | 0xec | 0xcc | 0xeb | 0xe0 | 0xe3 | 0xe4 | 0xe7
        | 0xe8 | 0xe9 => 0,
        _ => 0,
    }
}

fn process_poll(
    response: &[u8],
    state: &mut ValidatorState,
    transport: &mut SspTransport,
    events: &Sender<PaymentEvent>,
    decisions: &Receiver<EscrowDecision>,
    stop: &AtomicBool,
) -> KioskResult<PollDirective> {
    let status = response
        .first()
        .copied()
        .ok_or_else(|| hardware("NV9 Poll returned an empty response"))?;
    if status != RESPONSE_OK {
        emit(
            events,
            PaymentEvent::Notice {
                message: format!(
                    "NV9 Poll: SSP response 0x{status:02X} ({})",
                    response_description(status)
                ),
            },
        )?;
        return Ok(PollDirective::Continue);
    }

    let mut index = 1;
    while index < response.len() {
        let event_code = response[index];
        index += 1;
        let payload_len = poll_event_payload_len(event_code);
        let payload_end = index
            .checked_add(payload_len)
            .ok_or_else(|| hardware("malformed NV9 Poll: event payload length overflow"))?;
        if payload_end > response.len() {
            return Err(hardware(format!(
                "malformed NV9 Poll: event 0x{event_code:02X} missing {payload_len}-byte payload"
            )));
        }
        let payload = &response[index..payload_end];
        index = payload_end;

        match event_code {
            0x00 => {} // Empty/idle poll: ordinary, not an error.
            0xf1 => {
                // Slave Reset loses sequence/enabled state; resynchronize and
                // repeat Setup/Inhibits/Enable before accepting more money.
                state.escrow_value = None;
                emit_notice(events, "NV9 slave reset; reinitializing validator")?;
                return Ok(PollDirective::Reinitialize);
            }
            0xef => {
                // READ is followed by a one-based channel. Channel zero means
                // recognition/transport is still in progress; nonzero is escrow.
                let channel = payload[0];
                if channel == 0 {
                    continue;
                }

                let value = state
                    .channel_values
                    .get(usize::from(channel - 1))
                    .copied()
                    .unwrap_or(0);
                if value <= 0 {
                    emit_notice(
                        events,
                        format!("NV9 reported unknown or inhibited channel {channel}"),
                    )?;
                    continue;
                }
                if state.escrow_value.is_some() {
                    emit_notice(events, "NV9 reported a second escrow note while one is pending")?;
                    continue;
                }

                state.escrow_value = Some(value);
                emit(events, PaymentEvent::NoteInEscrow { value_rsd: value })?;
                match wait_for_decision(decisions, stop)? {
                    DecisionWait::Accept => {
                        // NV9 proceeds from escrow to stacking; credit only on
                        // the later 0xEB Note Stacked event, never at decision time.
                    }
                    DecisionWait::Reject => {
                        // 0x08 immediately returns the held note. Keep its value
                        // until 0xEC confirms that the visitor actually received it.
                        let reject_response = match transport.command(CMD_REJECT, &[]) {
                            Ok(response) => response,
                            Err(_) => {
                                transport.sync()?;
                                transport.command(CMD_REJECT, &[])?
                            }
                        };
                        acknowledge_or_notice(&reject_response, "Reject", events)?;
                    }
                    DecisionWait::Stop => {
                        // Avoid leaving a banknote captured when the session stops.
                        let _ = transport.command(CMD_REJECT, &[]);
                        return Ok(PollDirective::Stop);
                    }
                }
            }
            0xee => {
                // 0xEE Note Credit carries a channel, but 0xEB Note Stacked is
                // this driver's sole credit point. Consume it informationally.
                let _channel = payload[0];
            }
            0xed => {
                // 0xED Note Rejecting: the note is still moving toward the bezel.
                emit_notice(events, "NV9 note rejecting")?;
            }
            0xec => {
                // 0xEC Note Rejected: return completed at the customer bezel.
                if let Some(value) = state.escrow_value.take() {
                    emit(events, PaymentEvent::NoteReturned { value_rsd: value })?;
                } else {
                    emit_notice(events, "NV9 reported Note Rejected without tracked escrow")?;
                }
            }
            0xcc => {
                // Canonical 0xCC: accepted note is moving into the stacker.
                emit_notice(events, "NV9 note stacking")?;
            }
            0xeb => {
                // Canonical 0xEB: stack completed. This is the sole credit point.
                if let Some(value) = state.escrow_value.take() {
                    state.total_inserted_rsd = state
                        .total_inserted_rsd
                        .checked_add(value)
                        .ok_or_else(|| hardware("NV9 inserted-total overflow"))?;
                    emit(
                        events,
                        PaymentEvent::Credited {
                            value_rsd: value,
                            total_inserted_rsd: state.total_inserted_rsd,
                        },
                    )?;
                } else {
                    emit_notice(events, "NV9 reported Note Stacked without tracked escrow")?;
                }
            }
            0xe1 => {
                // 0xE1 Note Cleared From Front carries the cleared channel.
                if let Some(value) = state.escrow_value.take() {
                    emit(events, PaymentEvent::NoteReturned { value_rsd: value })?;
                } else {
                    emit_notice(events, "NV9 cleared a note from the front path")?;
                }
            }
            0xe2 => {
                // 0xE2 Note Cleared Into Cashbox carries a channel but is not
                // proof of payment credit; staff must reconcile the captured note.
                state.escrow_value = None;
                emit(
                    events,
                    PaymentEvent::Error {
                        message: "Novčanica povučena u kasu bez naplate — pozvati osoblje"
                            .to_string(),
                    },
                )?;
            }
            0xe3 => {
                // 0xE3 Cash Box Removed: link remains pollable, stacking unavailable.
                emit_notice(events, "NV9 cash box removed; stacking unavailable")?;
            }
            0xe4 => {
                // 0xE4 Cash Box Replaced.
                emit_notice(events, "NV9 cash box replaced")?;
            }
            0xe8 => {
                // Canonical 0xE8 in this NV9 contract: validator disabled.
                emit_notice(events, "NV9 validator disabled")?;
            }
            0xe6 => {
                // 0xE6 Fraud Attempt / unsafe jam carries the affected channel.
                emit_notice(
                    events,
                    format!("NV9 fraud-attempt/unsafe-jam warning on channel {}", payload[0]),
                )?;
            }
            0xe7 => {
                // 0xE7 Stacker Full: acceptance is unavailable, but polling lives.
                emit_notice(events, "NV9 stacker full; empty cash box")?;
            }
            0xe0 => {
                // Note-path/jam-family warning; conservatively keep polling.
                emit_notice(events, "NV9 note-path warning (event 0xE0)")?;
            }
            0xe9 => {
                // Meaning is firmware-dependent; retain the exact code for operators.
                emit_notice(events, "NV9 path warning (event 0xE9)")?;
            }
            unknown => {
                // Unknown rare events are conservatively non-fatal: firmware
                // variants allocate secondary event codes differently.
                emit_notice(
                    events,
                    format!("NV9 unrecognized Poll event 0x{unknown:02X}"),
                )?;
            }
        }
    }

    Ok(PollDirective::Continue)
}

fn poll_loop(
    transport: &mut SspTransport,
    setup: SetupData,
    events: &Sender<PaymentEvent>,
    decisions: &Receiver<EscrowDecision>,
    stop: &AtomicBool,
) -> KioskResult<()> {
    let mut state = ValidatorState {
        channel_values: setup.channel_values,
        escrow_value: None,
        total_inserted_rsd: 0,
    };

    while !stop.load(Ordering::Acquire) {
        let poll_response = match transport.command(CMD_POLL, &[]) {
            Ok(response) => response,
            Err(first_error) => {
                // Each command already had bounded retransmits. One full SYNC +
                // setup recovery is attempted before declaring the link lost.
                emit_notice(
                    events,
                    format!("NV9 Poll failed; attempting resync: {first_error}"),
                )?;
                let recovered = initialize_validator(transport, events).map_err(|recovery_error| {
                    hardware(format!(
                        "NV9 Poll failed and resync was unsuccessful: {recovery_error}"
                    ))
                })?;
                state.channel_values = recovered.channel_values;
                state.escrow_value = None;
                continue;
            }
        };

        match process_poll(
            &poll_response,
            &mut state,
            transport,
            events,
            decisions,
            stop,
        )? {
            PollDirective::Continue => {}
            PollDirective::Reinitialize => {
                let recovered = initialize_validator(transport, events)?;
                state.channel_values = recovered.channel_values;
            }
            PollDirective::Stop => break,
        }

        if stop.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    Ok(())
}

/// Opens the port, runs Sync + Setup + Enable, then loops polling.
/// Emits PaymentEvent through `events`, reads EscrowDecision through `decisions`.
/// Returns when `stop` becomes true, or on a fatal error.
pub fn run_validator(
    cfg: Nv9Config,
    events: crossbeam_channel::Sender<crate::models::PaymentEvent>,
    decisions: crossbeam_channel::Receiver<EscrowDecision>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> crate::models::KioskResult<()> {
    let port = match serialport::new(&cfg.port, cfg.baud)
        .data_bits(DataBits::Eight)
        .flow_control(FlowControl::None)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .timeout(SERIAL_TIMEOUT)
        .open()
        .map_err(|error| hardware(format!("cannot open NV9 port {}: {error}", cfg.port)))
    {
        Ok(port) => port,
        Err(error) => {
            report_fatal(&events, &error);
            return Err(error);
        }
    };

    let mut transport = SspTransport::new(port);
    let setup = match initialize_validator(&mut transport, &events) {
        Ok(setup) => setup,
        Err(error) => {
            report_fatal(&events, &error);
            return Err(error);
        }
    };
    if let Err(error) = emit(&events, PaymentEvent::Connected) {
        report_fatal(&events, &error);
        return Err(error);
    }

    let run_result = poll_loop(&mut transport, setup, &events, &decisions, &stop);

    // 0x09 disables acceptance before the serial handle is dropped on every
    // normal exit and every post-initialization failure path.
    let disable_result = transport
        .command(CMD_DISABLE, &[])
        .and_then(|response| acknowledge_or_notice(&response, "Disable", &events).map(|_| ()));

    match run_result {
        Err(error) => {
            report_fatal(&events, &error);
            let _ = events.send(PaymentEvent::Disconnected);
            Err(error)
        }
        Ok(()) => match disable_result {
            Err(error) => {
                report_fatal(&events, &error);
                let _ = events.send(PaymentEvent::Disconnected);
                Err(error)
            }
            Ok(()) => {
                match emit(&events, PaymentEvent::Disconnected) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        report_fatal(&events, &error);
                        Err(error)
                    }
                }
            }
        },
    }
}

/// Quick availability check: open at NV9's standard 9600 baud, synchronize,
/// request the four-byte big-endian device serial, then close the port.
pub fn probe(port: &str) -> crate::models::KioskResult<String> {
    let serial = serialport::new(port, 9_600)
        .data_bits(DataBits::Eight)
        .flow_control(FlowControl::None)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .timeout(SERIAL_TIMEOUT)
        .open()
        .map_err(|error| hardware(format!("cannot open NV9 port {port}: {error}")))?;

    let mut transport = SspTransport::new(serial);
    transport.sync()?;
    let response = transport.command(CMD_GET_SERIAL, &[])?;
    let status = response
        .first()
        .copied()
        .ok_or_else(|| hardware("NV9 Get Serial Number returned an empty response"))?;

    if status == RESPONSE_UNKNOWN_COMMAND {
        return Ok(format!(
            "NV9 on {port} (serial-number command unsupported by firmware)"
        ));
    }
    if status != RESPONSE_OK {
        return Ok(format!(
            "NV9 on {port} (serial unavailable: SSP 0x{status:02X})"
        ));
    }
    if response.len() < 5 {
        return Err(hardware(
            "malformed NV9 Get Serial Number response (expected four bytes)",
        ));
    }

    let number = u32::from_be_bytes([response[1], response[2], response[3], response[4]]);

    // Diagnostic dump so real-unit denomination parsing can be verified from the admin
    // Devices tab: raw Setup Request bytes + how we currently interpret each channel.
    let setup = transport.command(CMD_SETUP_REQUEST, &[]).unwrap_or_default();
    let hex: String = setup
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    let channels = match parse_setup_response(&setup) {
        Ok(data) => data
            .channel_values
            .iter()
            .enumerate()
            .map(|(i, v)| format!("k{}={}", i + 1, v))
            .collect::<Vec<_>>()
            .join(" "),
        Err(error) => format!("parse: {error}"),
    };
    Ok(format!(
        "NV9 serial {number} on {port} | kanali: {channels} | setup: {hex}"
    ))
}

/// List serial device names for the settings UI. USB metadata is not equally
/// available on Windows/macOS, so retain all ports rather than hiding a valid NV9.
pub fn list_ports() -> Vec<String> {
    let mut ports = match serialport::available_ports() {
        Ok(ports) => ports
            .into_iter()
            .map(|port| port.port_name)
            .collect::<Vec<_>>(),
        Err(error) => {
            log::warn!("failed to enumerate serial ports for NV9: {error}");
            Vec::new()
        }
    };
    ports.sort();
    ports.dedup();
    ports
}

#[cfg(feature = "simulate")]
pub fn run_simulator(
    _cfg: Nv9Config,
    events: crossbeam_channel::Sender<crate::models::PaymentEvent>,
    decisions: crossbeam_channel::Receiver<EscrowDecision>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> crate::models::KioskResult<()> {
    const DENOMINATIONS: [i64; 4] = [100, 200, 500, 1_000];
    const NOTE_INTERVAL: Duration = Duration::from_secs(2);

    emit(&events, PaymentEvent::Connected)?;
    let mut next_note = Instant::now() + NOTE_INTERVAL;
    let mut denomination_index = 0_usize;
    let mut total_inserted_rsd = 0_i64;

    while !stop.load(Ordering::Acquire) {
        if Instant::now() < next_note {
            std::thread::sleep(ESCROW_WAIT_SLICE);
            continue;
        }

        let value = DENOMINATIONS[denomination_index];
        denomination_index = (denomination_index + 1) % DENOMINATIONS.len();
        emit(&events, PaymentEvent::NoteInEscrow { value_rsd: value })?;

        match wait_for_decision(&decisions, &stop)? {
            DecisionWait::Accept => {
                total_inserted_rsd = total_inserted_rsd
                    .checked_add(value)
                    .ok_or_else(|| hardware("NV9 simulator inserted-total overflow"))?;
                emit(
                    &events,
                    PaymentEvent::Credited {
                        value_rsd: value,
                        total_inserted_rsd,
                    },
                )?;
            }
            DecisionWait::Reject => {
                emit(&events, PaymentEvent::NoteReturned { value_rsd: value })?;
            }
            DecisionWait::Stop => break,
        }
        next_note = Instant::now() + NOTE_INTERVAL;
    }

    emit(&events, PaymentEvent::Disconnected)?;
    Ok(())
}

fn emit(events: &Sender<PaymentEvent>, event: PaymentEvent) -> KioskResult<()> {
    events
        .send(event)
        .map_err(|_| hardware("NV9 payment-event receiver disconnected"))
}

fn emit_notice(events: &Sender<PaymentEvent>, message: impl Into<String>) -> KioskResult<()> {
    let message = message.into();
    log::warn!("{message}");
    emit(events, PaymentEvent::Notice { message })
}

fn report_fatal(events: &Sender<PaymentEvent>, error: &KioskError) {
    let message = error.to_string();
    log::error!("{message}");
    let _ = events.send(PaymentEvent::Error { message });
}

fn hardware(message: impl Into<String>) -> KioskError {
    KioskError::Hardware(message.into())
}
