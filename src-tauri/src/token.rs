use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::models::{KioskError, KioskResult};

type HmacSha256 = Hmac<Sha256>;

const APP_MASK: [u8; 32] = [
    0x6d, 0x21, 0xa7, 0x4c, 0x93, 0x18, 0xe2, 0x5f, 0xb4, 0x70, 0x0d, 0xc8, 0x36, 0xfa, 0x81, 0x2b,
    0x57, 0xde, 0x09, 0x64, 0xbb, 0x42, 0xf1, 0x7a, 0x15, 0x8c, 0xd3, 0x20, 0x9e, 0x4a, 0x76, 0xcd,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketClaims {
    pub id: String,
    pub type_code: String,
    pub price_rsd: i64,
    pub issued_at: i64,
}

pub fn sign(secret: &[u8], c: &TicketClaims) -> String {
    let prefix = signed_prefix(c);
    let mut mac =
        HmacSha256::new_from_slice(secret).expect("HMAC-SHA256 accepts keys of any length");
    mac.update(prefix.as_bytes());
    let encoded_mac = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{prefix}|{encoded_mac}")
}

/// Offline verification of a ticket QR token. Not called by the kiosk itself —
/// this is the API the entrance-gate scanner app links against to validate tickets
/// without network access, using the same HMAC secret.
#[allow(dead_code)]
pub fn verify(secret: &[u8], token: &str) -> KioskResult<TicketClaims> {
    let fields: Vec<&str> = token.split('|').collect();
    if fields.len() != 7 {
        return Err(KioskError::Token(format!(
            "neispravan format: očekivano 7 polja, dobijeno {}",
            fields.len()
        )));
    }
    if fields[0] != "MMM" {
        return Err(KioskError::Token("neispravan prefiks tokena".into()));
    }
    if fields[1] != "v1" {
        return Err(KioskError::Token("nepodržana verzija tokena".into()));
    }
    if fields[2].is_empty() {
        return Err(KioskError::Token("identifikator tiketa je prazan".into()));
    }
    if fields[3].is_empty() {
        return Err(KioskError::Token("tip tiketa je prazan".into()));
    }

    let price_rsd = fields[4]
        .parse::<i64>()
        .map_err(|_| KioskError::Token("cena nije ispravan i64 broj".into()))?;
    let issued_at = fields[5]
        .parse::<i64>()
        .map_err(|_| KioskError::Token("vreme izdavanja nije ispravan i64 broj".into()))?;
    let supplied_mac = URL_SAFE_NO_PAD
        .decode(fields[6])
        .map_err(|_| KioskError::Token("MAC nije ispravan base64url bez dopune".into()))?;

    let prefix = fields[..6].join("|");
    let mut mac =
        HmacSha256::new_from_slice(secret).expect("HMAC-SHA256 accepts keys of any length");
    mac.update(prefix.as_bytes());
    let expected_mac = mac.finalize().into_bytes();

    if !constant_time_eq(expected_mac.as_slice(), &supplied_mac) {
        return Err(KioskError::Token("MAC provera nije uspela".into()));
    }

    Ok(TicketClaims {
        id: fields[2].to_owned(),
        type_code: fields[3].to_owned(),
        price_rsd,
        issued_at,
    })
}

pub fn obfuscate(raw: &[u8]) -> Vec<u8> {
    xor_mask(raw)
}

pub fn deobfuscate(stored: &[u8]) -> Vec<u8> {
    xor_mask(stored)
}

pub fn new_secret() -> Vec<u8> {
    let mut secret = Vec::with_capacity(32);
    secret.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    secret.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    secret
}

fn signed_prefix(c: &TicketClaims) -> String {
    format!(
        "MMM|v1|{}|{}|{}|{}",
        c.id, c.type_code, c.price_rsd, c.issued_at
    )
}

fn xor_mask(input: &[u8]) -> Vec<u8> {
    input
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ APP_MASK[index % APP_MASK.len()])
        .collect()
}

#[allow(dead_code)] // used by verify() — the gate-side API
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut difference = 0_u8;
    for (&left_byte, &right_byte) in left.iter().zip(right) {
        difference |= left_byte ^ right_byte;
    }
    difference == 0
}
