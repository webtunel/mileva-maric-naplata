use std::time::Duration;

use chrono::{Local, TimeZone};
use qrcode::{types::Color, QrCode};
use rusb::{Context, Device, DeviceDescriptor, DeviceHandle, Direction, TransferType, UsbContext};

use crate::models::KioskError;

const LIBUSB_CLASS_PRINTER: u8 = 7;
const USE_NATIVE_QR: bool = true;
const USB_TIMEOUT: Duration = Duration::from_secs(5);
const USB_WRITE_CHUNK_SIZE: usize = 16 * 1024;
const RASTER_QUIET_ZONE_MODULES: usize = 4;
const RASTER_MAX_SCALE: usize = 4;
const RASTER_MAX_WIDTH_DOTS: usize = 384;

pub struct PrinterTarget {
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
}

struct EndpointSelection {
    configuration: u8,
    interface: u8,
    alternate_setting: u8,
    endpoint: u8,
}

struct EndpointOptions {
    printer: Option<EndpointSelection>,
    bulk_out: Option<EndpointSelection>,
}

struct DeviceCandidate {
    device: Device<Context>,
    descriptor: DeviceDescriptor,
    endpoint: EndpointSelection,
}

struct OpenPrinter {
    handle: DeviceHandle<Context>,
    endpoint: u8,
    vendor_id: u16,
    product_id: u16,
}

pub fn print_tickets(
    target: &PrinterTarget,
    museum: &str,
    tickets: &[crate::models::PrintedTicket],
    before_mm: u32,
    after_mm: u32,
    width_mm: u32,
) -> crate::models::KioskResult<()> {
    let printer = open_printer(target)?;

    for ticket in tickets {
        let job = build_ticket_job(museum, ticket, before_mm, after_mm, width_mm)?;
        write_bulk_all(&printer, &job)?;
    }

    Ok(())
}

pub fn probe(target: &PrinterTarget) -> crate::models::KioskResult<String> {
    let printer = open_printer(target)?;
    Ok(format!(
        "USB {:04x}:{:04x} (bulk OUT ok)",
        printer.vendor_id, printer.product_id
    ))
}

/// Print via a serial (virtual COM) port such as the Bixolon BXLVCOM4USB virtual port.
/// Reuses the exact same ESC/POS job bytes as the USB path — just a different transport.
pub fn print_tickets_serial(
    port: &str,
    museum: &str,
    tickets: &[crate::models::PrintedTicket],
    before_mm: u32,
    after_mm: u32,
    width_mm: u32,
) -> crate::models::KioskResult<()> {
    let mut serial = serialport::new(port, 9600)
        .timeout(std::time::Duration::from_secs(5))
        .open()
        .map_err(|error| {
            KioskError::Print(format!("ne mogu da otvorim printer port {port}: {error}"))
        })?;
    for ticket in tickets {
        let job = build_ticket_job(museum, ticket, before_mm, after_mm, width_mm)?;
        std::io::Write::write_all(&mut *serial, &job)
            .map_err(|error| KioskError::Print(format!("greška pri štampi na {port}: {error}")))?;
    }
    let _ = std::io::Write::flush(&mut *serial);
    Ok(())
}

pub fn probe_serial(port: &str) -> crate::models::KioskResult<String> {
    serialport::new(port, 9600)
        .timeout(std::time::Duration::from_millis(600))
        .open()
        .map_err(|error| KioskError::Print(format!("printer port {port} nedostupan: {error}")))?;
    Ok(format!("Serijski printer na {port} (otvoren)"))
}

// --- Windows print-spooler transport (raw ESC/POS to a named printer, e.g. the
// manufacturer's "BIXOLON SRP-Q300" driver). Most robust on Windows: no Zadig, no
// COM-port fighting — the native driver owns the USB. ---

#[cfg(windows)]
mod winspool {
    use std::os::raw::c_void;
    pub type Handle = *mut c_void;
    #[repr(C)]
    pub struct DocInfo1W {
        pub p_doc_name: *mut u16,
        pub p_output_file: *mut u16,
        pub p_datatype: *mut u16,
    }
    #[link(name = "winspool")]
    extern "system" {
        pub fn OpenPrinterW(name: *mut u16, handle: *mut Handle, defaults: *mut c_void) -> i32;
        pub fn StartDocPrinterW(handle: Handle, level: u32, doc_info: *mut u8) -> u32;
        pub fn StartPagePrinter(handle: Handle) -> i32;
        pub fn WritePrinter(handle: Handle, buf: *mut c_void, len: u32, written: *mut u32) -> i32;
        pub fn EndPagePrinter(handle: Handle) -> i32;
        pub fn EndDocPrinter(handle: Handle) -> i32;
        pub fn ClosePrinter(handle: Handle) -> i32;
    }
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
pub fn print_tickets_windows(
    printer_name: &str,
    museum: &str,
    tickets: &[crate::models::PrintedTicket],
    before_mm: u32,
    after_mm: u32,
    width_mm: u32,
) -> crate::models::KioskResult<()> {
    let mut bytes = Vec::new();
    for ticket in tickets {
        bytes.extend_from_slice(&build_ticket_job(museum, ticket, before_mm, after_mm, width_mm)?);
    }
    let mut name = wide(printer_name);
    let mut doc_name = wide("Ulaznica");
    let mut datatype = wide("RAW");
    unsafe {
        let mut handle: winspool::Handle = std::ptr::null_mut();
        if winspool::OpenPrinterW(name.as_mut_ptr(), &mut handle, std::ptr::null_mut()) == 0 {
            return Err(KioskError::Print(format!(
                "ne mogu da otvorim Windows štampač '{printer_name}' (proveri ime i da li je funkcionalan)"
            )));
        }
        let mut doc = winspool::DocInfo1W {
            p_doc_name: doc_name.as_mut_ptr(),
            p_output_file: std::ptr::null_mut(),
            p_datatype: datatype.as_mut_ptr(),
        };
        let job = winspool::StartDocPrinterW(handle, 1, &mut doc as *mut _ as *mut u8);
        if job == 0 {
            winspool::ClosePrinter(handle);
            return Err(KioskError::Print("StartDocPrinter nije uspeo".into()));
        }
        winspool::StartPagePrinter(handle);
        let mut written: u32 = 0;
        let ok = winspool::WritePrinter(
            handle,
            bytes.as_ptr() as *mut std::os::raw::c_void,
            bytes.len() as u32,
            &mut written,
        );
        winspool::EndPagePrinter(handle);
        winspool::EndDocPrinter(handle);
        winspool::ClosePrinter(handle);
        if ok == 0 || (written as usize) < bytes.len() {
            return Err(KioskError::Print("štampač nije primio sve podatke".into()));
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn probe_windows(printer_name: &str) -> crate::models::KioskResult<String> {
    let mut name = wide(printer_name);
    unsafe {
        let mut handle: winspool::Handle = std::ptr::null_mut();
        if winspool::OpenPrinterW(name.as_mut_ptr(), &mut handle, std::ptr::null_mut()) == 0 {
            return Err(KioskError::Print(format!(
                "Windows štampač '{printer_name}' nije dostupan"
            )));
        }
        winspool::ClosePrinter(handle);
    }
    Ok(format!("Windows štampač '{printer_name}' (spooler ok)"))
}

#[cfg(not(windows))]
pub fn print_tickets_windows(
    _printer_name: &str,
    _museum: &str,
    _tickets: &[crate::models::PrintedTicket],
    _before_mm: u32,
    _after_mm: u32,
    _width_mm: u32,
) -> crate::models::KioskResult<()> {
    Err(KioskError::Print(
        "Windows štampa je dostupna samo na Windows-u".into(),
    ))
}

#[cfg(not(windows))]
pub fn probe_windows(_printer_name: &str) -> crate::models::KioskResult<String> {
    Err(KioskError::Print(
        "Windows štampa je dostupna samo na Windows-u".into(),
    ))
}

/// Opens the selected USB device and claims the interface containing its bulk OUT endpoint.
///
/// On Windows, raw libusb access requires the printer to use a WinUSB,
/// libusb-win32, or usbK driver installed via Zadig or a signed INF. The default
/// Windows USB Printer class driver does not allow libusb to claim the interface;
/// deployment and driver installation must handle that requirement.
fn open_printer(target: &PrinterTarget) -> crate::models::KioskResult<OpenPrinter> {
    let context = Context::new()
        .map_err(|error| KioskError::Print(format!("cannot initialize libusb: {error}")))?;
    let devices = context
        .devices()
        .map_err(|error| KioskError::Print(format!("cannot enumerate USB devices: {error}")))?;
    let candidate = select_device(&devices, target)?;
    let vid = candidate.descriptor.vendor_id();
    let pid = candidate.descriptor.product_id();
    let selection = candidate.endpoint;
    let handle = candidate.device.open().map_err(|error| {
        KioskError::Print(format!("cannot open USB {vid:04x}:{pid:04x}: {error}"))
    })?;

    let active_configuration = handle.active_configuration().map_err(|error| {
        KioskError::Print(format!(
            "cannot read active configuration for USB {vid:04x}:{pid:04x}: {error}"
        ))
    })?;
    if active_configuration != selection.configuration {
        handle
            .set_active_configuration(selection.configuration)
            .map_err(|error| {
                KioskError::Print(format!(
                    "cannot select configuration {} on USB {vid:04x}:{pid:04x}: {error}",
                    selection.configuration
                ))
            })?;
    }

    #[cfg(not(target_os = "windows"))]
    match handle.set_auto_detach_kernel_driver(true) {
        Ok(()) | Err(rusb::Error::NotSupported) => {}
        Err(error) => {
            return Err(KioskError::Print(format!(
                "cannot enable kernel-driver auto-detach for USB {vid:04x}:{pid:04x}: {error}"
            )));
        }
    }

    handle
        .claim_interface(selection.interface)
        .map_err(|error| {
            KioskError::Print(format!(
                "cannot claim interface {} on USB {vid:04x}:{pid:04x}: {error}",
                selection.interface
            ))
        })?;

    if selection.alternate_setting != 0 {
        handle
            .set_alternate_setting(selection.interface, selection.alternate_setting)
            .map_err(|error| {
                KioskError::Print(format!(
                    "cannot select alternate setting {} on interface {} of USB {vid:04x}:{pid:04x}: {error}",
                    selection.alternate_setting, selection.interface
                ))
            })?;
    }

    Ok(OpenPrinter {
        handle,
        endpoint: selection.endpoint,
        vendor_id: vid,
        product_id: pid,
    })
}

fn select_device(
    devices: &rusb::DeviceList<Context>,
    target: &PrinterTarget,
) -> crate::models::KioskResult<DeviceCandidate> {
    if let (Some(vendor_id), Some(product_id)) = (target.vendor_id, target.product_id) {
        return select_exact_device(devices, vendor_id, product_id);
    }

    select_automatic_device(devices)
}

fn select_exact_device(
    devices: &rusb::DeviceList<Context>,
    vendor_id: u16,
    product_id: u16,
) -> crate::models::KioskResult<DeviceCandidate> {
    let mut last_descriptor_error = None;

    for device in devices.iter() {
        let descriptor = match device.device_descriptor() {
            Ok(descriptor) => descriptor,
            Err(error) => {
                last_descriptor_error = Some(format!("cannot read USB device descriptor: {error}"));
                continue;
            }
        };
        if descriptor.vendor_id() != vendor_id || descriptor.product_id() != product_id {
            continue;
        }

        let endpoints = inspect_endpoints(&device, &descriptor, true)?;
        let endpoint = endpoints.printer.or(endpoints.bulk_out).ok_or_else(|| {
            KioskError::Print(format!(
                "USB {vendor_id:04x}:{product_id:04x} has no bulk OUT endpoint"
            ))
        })?;

        return Ok(DeviceCandidate {
            device,
            descriptor,
            endpoint,
        });
    }

    let detail = last_descriptor_error
        .map(|message| format!("; last USB error: {message}"))
        .unwrap_or_default();
    Err(KioskError::Print(format!(
        "USB printer {vendor_id:04x}:{product_id:04x} not found{detail}"
    )))
}

fn select_automatic_device(
    devices: &rusb::DeviceList<Context>,
) -> crate::models::KioskResult<DeviceCandidate> {
    let mut fallback = None;
    let mut last_descriptor_error = None;
    let mut last_config_error = None;

    for device in devices.iter() {
        let descriptor = match device.device_descriptor() {
            Ok(descriptor) => descriptor,
            Err(error) => {
                last_descriptor_error = Some(format!("cannot read USB device descriptor: {error}"));
                continue;
            }
        };

        let endpoints = match inspect_endpoints(&device, &descriptor, false) {
            Ok(endpoints) => endpoints,
            Err(error) => {
                last_config_error = Some(error.to_string());
                continue;
            }
        };

        if let Some(endpoint) = endpoints.printer {
            return Ok(DeviceCandidate {
                device,
                descriptor,
                endpoint,
            });
        }

        if fallback.is_none() {
            if let Some(endpoint) = endpoints.bulk_out {
                fallback = Some(DeviceCandidate {
                    device,
                    descriptor,
                    endpoint,
                });
            }
        }
    }

    if let Some(candidate) = fallback {
        return Ok(candidate);
    }

    let detail = last_config_error
        .or(last_descriptor_error)
        .map(|message| format!("; last USB error: {message}"))
        .unwrap_or_default();
    Err(KioskError::Print(format!(
        "no USB printer-class interface or bulk OUT endpoint found{detail}"
    )))
}

fn inspect_endpoints(
    device: &Device<Context>,
    descriptor: &DeviceDescriptor,
    fail_on_descriptor_error: bool,
) -> crate::models::KioskResult<EndpointOptions> {
    let vid = descriptor.vendor_id();
    let pid = descriptor.product_id();
    let mut printer = None;
    let mut bulk_out = None;
    let mut last_error = None;

    for config_index in 0..descriptor.num_configurations() {
        let config = match device.config_descriptor(config_index) {
            Ok(config) => config,
            Err(error) => {
                let mapped = KioskError::Print(format!(
                    "cannot read configuration {config_index} for USB {vid:04x}:{pid:04x}: {error}"
                ));
                if fail_on_descriptor_error {
                    return Err(mapped);
                }
                last_error = Some(mapped);
                continue;
            }
        };

        for interface in config.interfaces() {
            for interface_descriptor in interface.descriptors() {
                let endpoint = interface_descriptor
                    .endpoint_descriptors()
                    .find(|endpoint| {
                        endpoint.transfer_type() == TransferType::Bulk
                            && endpoint.direction() == Direction::Out
                    });
                let Some(endpoint) = endpoint else {
                    continue;
                };

                let selection = EndpointSelection {
                    configuration: config.number(),
                    interface: interface_descriptor.interface_number(),
                    alternate_setting: interface_descriptor.setting_number(),
                    endpoint: endpoint.address(),
                };

                if interface_descriptor.class_code() == LIBUSB_CLASS_PRINTER && printer.is_none() {
                    printer = Some(selection);
                } else if bulk_out.is_none() {
                    bulk_out = Some(selection);
                }
            }
        }
    }

    if printer.is_none() && bulk_out.is_none() {
        if let Some(error) = last_error {
            return Err(error);
        }
    }

    Ok(EndpointOptions { printer, bulk_out })
}

fn write_bulk_all(printer: &OpenPrinter, data: &[u8]) -> crate::models::KioskResult<()> {
    for chunk in data.chunks(USB_WRITE_CHUNK_SIZE) {
        let written = printer
            .handle
            .write_bulk(printer.endpoint, chunk, USB_TIMEOUT)
            .map_err(|error| {
                KioskError::Print(format!(
                    "bulk write to endpoint 0x{:02x} on USB {:04x}:{:04x} failed: {error}",
                    printer.endpoint, printer.vendor_id, printer.product_id
                ))
            })?;

        if written != chunk.len() {
            return Err(KioskError::Print(format!(
                "short bulk write to endpoint 0x{:02x} on USB {:04x}:{:04x}: wrote {written} of {} bytes",
                printer.endpoint,
                printer.vendor_id,
                printer.product_id,
                chunk.len()
            )));
        }
    }

    Ok(())
}

/// Feed `mm` millimeters of blank paper via ESC J (n/203 inch, ~8 dots/mm at 203dpi).
/// ESC J caps at 255 dots (~32mm) per call, so split larger feeds across several calls.
fn append_feed(job: &mut Vec<u8>, mm: u32) {
    let mut dots = mm.saturating_mul(8); // ~8 dots per mm at 203 dpi
    while dots > 0 {
        let chunk = dots.min(255) as u8;
        job.extend_from_slice(&[0x1b, 0x4a, chunk]); // ESC J n
        dots -= u32::from(chunk);
    }
}

fn build_ticket_job(
    museum: &str,
    ticket: &crate::models::PrintedTicket,
    before_mm: u32,
    after_mm: u32,
    width_mm: u32,
) -> crate::models::KioskResult<Vec<u8>> {
    let issued_at = Local
        .timestamp_opt(ticket.issued_at, 0)
        .single()
        .ok_or_else(|| {
            KioskError::Print(format!(
                "ticket {} has invalid issued_at timestamp {}",
                ticket.id, ticket.issued_at
            ))
        })?;
    let short_code_reversed: String = ticket.id.chars().rev().take(8).collect();
    let short_code: String = short_code_reversed.chars().rev().collect();

    let mut job = Vec::new();
    job.extend_from_slice(&[0x1b, 0x40]); // ESC @  (init)
    job.extend_from_slice(&[0x1b, 0x74, 0x12]); // ESC t 18 (CP852 code page)
    // GS W — set print area width in dots so centered text/QR align to the loaded paper.
    let width_dots: u16 = if width_mm <= 58 { 384 } else { 576 };
    let [wl, wh] = width_dots.to_le_bytes();
    job.extend_from_slice(&[0x1d, 0x57, wl, wh]);
    job.extend_from_slice(&[0x1b, 0x61, 0x01]);
    job.extend_from_slice(&[0x1d, 0x21, 0x11]);
    job.extend_from_slice(&[0x1b, 0x45, 0x01]);
    append_text_line(&mut job, museum);

    job.extend_from_slice(&[0x1d, 0x21, 0x00]);
    job.extend_from_slice(&[0x1b, 0x45, 0x00]);
    append_text_line(&mut job, &ticket.label);
    append_text_line(&mut job, &format!("{} RSD", ticket.price_rsd));
    append_text_line(&mut job, &issued_at.format("%d.%m.%Y %H:%M").to_string());
    append_text_line(&mut job, &format!("#{}", short_code.to_uppercase()));
    job.push(b'\n');

    if USE_NATIVE_QR {
        append_native_qr(&mut job, ticket.qr_token.as_bytes())?;
    } else {
        append_raster_qr(&mut job, ticket.qr_token.as_bytes())?;
    }

    // Feed ~6 lines to clear the cutter gap so the blade lands just below the content,
    // cut, then feed ~28mm of BLANK paper after the cut (no cut) so a clean 2-3cm lead
    // sticks out of the printer, ready for the next ticket and easy to grab.
    // Feed the configurable blank margin (mm) so the blade clears the content, cut, then
    // feed the configurable tail (mm) after the cut. Both are admin-adjustable.
    append_feed(&mut job, before_mm);
    job.extend_from_slice(&[0x1d, 0x56, 0x01]); // GS V 1 partial cut
    append_feed(&mut job, after_mm);
    Ok(job)
}

fn append_text_line(output: &mut Vec<u8>, text: &str) {
    output.extend_from_slice(&text_to_cp852(text));
    output.push(b'\n');
}

fn append_native_qr(output: &mut Vec<u8>, data: &[u8]) -> crate::models::KioskResult<()> {
    append_gs_k(output, 0x31, 0x41, &[0x32, 0x00])?;
    append_gs_k(output, 0x31, 0x43, &[0x06])?;
    append_gs_k(output, 0x31, 0x45, &[0x32])?;

    let store_capacity = data.len().checked_add(1).ok_or_else(|| {
        KioskError::Print("QR token is too large for an ESC/POS QR frame".to_string())
    })?;
    let mut store_params = Vec::with_capacity(store_capacity);
    store_params.push(0x30);
    store_params.extend_from_slice(data);
    append_gs_k(output, 0x31, 0x50, &store_params)?;
    append_gs_k(output, 0x31, 0x51, &[0x30])
}

fn append_gs_k(
    output: &mut Vec<u8>,
    cn: u8,
    function: u8,
    params: &[u8],
) -> crate::models::KioskResult<()> {
    let payload_length = params
        .len()
        .checked_add(2)
        .ok_or_else(|| KioskError::Print("ESC/POS GS ( k payload length overflow".to_string()))?;
    let payload_length = u16::try_from(payload_length).map_err(|_| {
        KioskError::Print(format!(
            "ESC/POS GS ( k payload is too large: {payload_length} bytes"
        ))
    })?;
    let [p_l, p_h] = payload_length.to_le_bytes();

    output.extend_from_slice(&[0x1d, 0x28, 0x6b, p_l, p_h, cn, function]);
    output.extend_from_slice(params);
    Ok(())
}

fn append_raster_qr(output: &mut Vec<u8>, data: &[u8]) -> crate::models::KioskResult<()> {
    let code = QrCode::new(data)
        .map_err(|error| KioskError::Print(format!("cannot encode QR token: {error}")))?;
    let source_width = code.width();
    if source_width == 0 {
        return Err(KioskError::Print(
            "QR encoder returned an empty module grid".to_string(),
        ));
    }

    let quiet_zone = RASTER_QUIET_ZONE_MODULES
        .checked_mul(2)
        .ok_or_else(|| KioskError::Print("QR quiet-zone size overflow".to_string()))?;
    let rendered_modules = source_width
        .checked_add(quiet_zone)
        .ok_or_else(|| KioskError::Print("QR module width overflow".to_string()))?;
    let scale = (RASTER_MAX_WIDTH_DOTS / rendered_modules).clamp(1, RASTER_MAX_SCALE);
    let pixel_width = rendered_modules
        .checked_mul(scale)
        .ok_or_else(|| KioskError::Print("QR raster width overflow".to_string()))?;
    let pixel_height = pixel_width;
    let width_bytes = pixel_width
        .checked_add(7)
        .ok_or_else(|| KioskError::Print("QR raster row width overflow".to_string()))?
        / 8;
    let raster_length = width_bytes
        .checked_mul(pixel_height)
        .ok_or_else(|| KioskError::Print("QR raster buffer size overflow".to_string()))?;
    let width_bytes_u16 = u16::try_from(width_bytes).map_err(|_| {
        KioskError::Print(format!("QR raster row is too wide: {width_bytes} bytes"))
    })?;
    let pixel_height_u16 = u16::try_from(pixel_height)
        .map_err(|_| KioskError::Print(format!("QR raster is too tall: {pixel_height} dots")))?;

    let colors = code.to_colors();
    let mut raster = vec![0_u8; raster_length];
    let quiet_start = RASTER_QUIET_ZONE_MODULES;
    let quiet_end = quiet_start
        .checked_add(source_width)
        .ok_or_else(|| KioskError::Print("QR quiet-zone boundary overflow".to_string()))?;

    for pixel_y in 0..pixel_height {
        let module_y = pixel_y / scale;
        if module_y < quiet_start || module_y >= quiet_end {
            continue;
        }

        for pixel_x in 0..pixel_width {
            let module_x = pixel_x / scale;
            if module_x < quiet_start || module_x >= quiet_end {
                continue;
            }

            let source_y = module_y - quiet_start;
            let source_x = module_x - quiet_start;
            let source_row = source_y
                .checked_mul(source_width)
                .ok_or_else(|| KioskError::Print("QR source-row offset overflow".to_string()))?;
            let source_index = source_row
                .checked_add(source_x)
                .ok_or_else(|| KioskError::Print("QR source index overflow".to_string()))?;
            let color = colors.get(source_index).ok_or_else(|| {
                KioskError::Print("QR encoder returned an incomplete module grid".to_string())
            })?;
            if *color != Color::Dark {
                continue;
            }

            let raster_row = pixel_y
                .checked_mul(width_bytes)
                .ok_or_else(|| KioskError::Print("QR raster-row offset overflow".to_string()))?;
            let raster_index = raster_row
                .checked_add(pixel_x / 8)
                .ok_or_else(|| KioskError::Print("QR raster index overflow".to_string()))?;
            let target = raster.get_mut(raster_index).ok_or_else(|| {
                KioskError::Print("QR raster index is outside the output buffer".to_string())
            })?;
            *target |= 0x80 >> (pixel_x % 8);
        }
    }

    let [x_l, x_h] = width_bytes_u16.to_le_bytes();
    let [y_l, y_h] = pixel_height_u16.to_le_bytes();
    output.extend_from_slice(&[0x1d, 0x76, 0x30, 0x00, x_l, x_h, y_l, y_h]);
    output.extend_from_slice(&raster);
    Ok(())
}

fn text_to_cp852(text: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(text.len());

    for character in text.chars() {
        if character.is_ascii() {
            output.push(character as u8);
            continue;
        }

        let encoded = match character {
            'Ç' => Some(0x80),
            'ü' => Some(0x81),
            'é' => Some(0x82),
            'â' => Some(0x83),
            'ä' => Some(0x84),
            'ů' => Some(0x85),
            'ć' => Some(0x86),
            'ç' => Some(0x87),
            'ł' => Some(0x88),
            'ë' => Some(0x89),
            'Ő' => Some(0x8a),
            'ő' => Some(0x8b),
            'î' => Some(0x8c),
            'Ź' => Some(0x8d),
            'Ä' => Some(0x8e),
            'Ć' => Some(0x8f),
            'É' => Some(0x90),
            'Ĺ' => Some(0x91),
            'ĺ' => Some(0x92),
            'ô' => Some(0x93),
            'ö' => Some(0x94),
            'Ľ' => Some(0x95),
            'ľ' => Some(0x96),
            'Ś' => Some(0x97),
            'ś' => Some(0x98),
            'Ö' => Some(0x99),
            'Ü' => Some(0x9a),
            'Ť' => Some(0x9b),
            'ť' => Some(0x9c),
            'Ł' => Some(0x9d),
            'č' => Some(0x9f),
            'á' => Some(0xa0),
            'í' => Some(0xa1),
            'ó' => Some(0xa2),
            'ú' => Some(0xa3),
            'Ą' => Some(0xa4),
            'ą' => Some(0xa5),
            'Ž' => Some(0xa6),
            'ž' => Some(0xa7),
            'Ę' => Some(0xa8),
            'ę' => Some(0xa9),
            'ź' => Some(0xab),
            'Č' => Some(0xac),
            'ş' => Some(0xad),
            'Á' => Some(0xb5),
            'Â' => Some(0xb6),
            'Ě' => Some(0xb7),
            'Ş' => Some(0xb8),
            'Ż' => Some(0xbd),
            'ż' => Some(0xbe),
            'Ă' => Some(0xc6),
            'ă' => Some(0xc7),
            'đ' => Some(0xd0),
            'Đ' => Some(0xd1),
            'Ď' => Some(0xd2),
            'Ë' => Some(0xd3),
            'ď' => Some(0xd4),
            'Ň' => Some(0xd5),
            'Í' => Some(0xd6),
            'Î' => Some(0xd7),
            'ě' => Some(0xd8),
            'Ó' => Some(0xe0),
            'ß' => Some(0xe1),
            'Ô' => Some(0xe2),
            'Ń' => Some(0xe3),
            'ń' => Some(0xe4),
            'ň' => Some(0xe5),
            'Š' => Some(0xe6),
            'š' => Some(0xe7),
            'Ŕ' => Some(0xe8),
            'Ú' => Some(0xe9),
            'ŕ' => Some(0xea),
            'Ű' => Some(0xeb),
            'ý' => Some(0xec),
            'Ý' => Some(0xed),
            'ţ' => Some(0xee),
            'ű' => Some(0xfb),
            'Ř' => Some(0xfc),
            'ř' => Some(0xfd),
            _ => None,
        };

        if let Some(byte) = encoded {
            output.push(byte);
        } else {
            append_ascii_fallback(&mut output, character);
        }
    }

    output
}

fn append_ascii_fallback(output: &mut Vec<u8>, character: char) {
    let fallback: &[u8] = match character {
        'à' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => b"a",
        'À' | 'Ã' | 'Å' | 'Ā' | 'Ă' | 'Ą' => b"A",
        'æ' => b"ae",
        'Æ' => b"AE",
        'ĉ' | 'ċ' => b"c",
        'Ĉ' | 'Ċ' => b"C",
        'ð' | 'ď' => b"d",
        'Ð' | 'Ď' => b"D",
        'è' | 'ê' | 'ē' | 'ė' | 'ę' | 'ě' => b"e",
        'È' | 'Ê' | 'Ē' | 'Ė' | 'Ę' | 'Ě' => b"E",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => b"g",
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' => b"G",
        'ĥ' | 'ħ' => b"h",
        'Ĥ' | 'Ħ' => b"H",
        'ì' | 'ï' | 'ĩ' | 'ī' | 'į' => b"i",
        'Ì' | 'Ï' | 'Ĩ' | 'Ī' | 'Į' => b"I",
        'ĵ' => b"j",
        'Ĵ' => b"J",
        'ķ' => b"k",
        'Ķ' => b"K",
        'ļ' => b"l",
        'Ļ' => b"L",
        'ñ' | 'ņ' => b"n",
        'Ñ' | 'Ņ' => b"N",
        'ò' | 'õ' | 'ø' | 'ō' => b"o",
        'Ò' | 'Õ' | 'Ø' | 'Ō' => b"O",
        'œ' => b"oe",
        'Œ' => b"OE",
        'ŗ' => b"r",
        'Ŗ' => b"R",
        'ŝ' | 'ş' | 'ș' => b"s",
        'Ŝ' | 'Ş' | 'Ș' => b"S",
        'þ' => b"th",
        'Þ' => b"TH",
        'ț' => b"t",
        'Ț' => b"T",
        'ù' | 'û' | 'ũ' | 'ū' | 'ŭ' | 'ų' => b"u",
        'Ù' | 'Û' | 'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ų' => b"U",
        'ŷ' | 'ÿ' => b"y",
        'Ŷ' | 'Ÿ' => b"Y",
        'ẑ' => b"z",
        'Ẑ' => b"Z",
        '–' | '—' | '−' => b"-",
        '‘' | '’' => b"'",
        '“' | '”' => b"\"",
        '\u{00a0}' => b" ",
        _ => b"?",
    };
    output.extend_from_slice(fallback);
}
