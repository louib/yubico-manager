use config::Command;
use std::time::Duration;
use std::{thread, slice};
use sec::{ crc16 };
use yubicoerror::YubicoError;
use libusb::{request_type, Direction, RequestType, Recipient, Context, DeviceHandle};

const DATA_SIZE: usize = 64;
const HID_GET_REPORT: u8 = 0x01;
const HID_SET_REPORT: u8 = 0x09;
const REPORT_TYPE_FEATURE: u16 = 0x03;

bitflags! {
    pub struct Flags: u8 {
        const SLOT_WRITE_FLAG = 0x80;
        const RESP_PENDING_FLAG = 0x40;
    }
}

pub fn open_device(context: &mut Context, vid: u16, pid: u16) -> Result<DeviceHandle, YubicoError> {
    let devices = match context.devices() {
        Ok(device) => device,
        Err(_) => {
            return Err(YubicoError::DeviceNotFound);
        }
    };

    for device in devices.iter() {
        let device_desc = match device.device_descriptor() {
            Ok(device) => device,
            Err(_) => {
                return Err(YubicoError::DeviceNotFound);
            }
        };

        if device_desc.vendor_id() == vid && device_desc.product_id() == pid {
            match device.open() {
                Ok(mut handle) => {
                    let config = match device.config_descriptor(0) {
                        Ok(c) => c,
                        Err(_) => continue
                    };

                    for interface in config.interfaces() {
                        for usb_int in interface.descriptors() {
                            match handle.kernel_driver_active(usb_int.interface_number()) {
                                Ok(true) => {
                                    handle.detach_kernel_driver(usb_int.interface_number())?;
                                },
                                _ => continue
                            };

                            if handle.active_configuration()? != config.number() {
                                handle.set_active_configuration(config.number())?;
                            }
                            handle.claim_interface(usb_int.interface_number())?;
                        }
                    }

                    return Ok(handle)
                },
                Err(_) => {
                    return Err(YubicoError::OpenDeviceError);
                }
            }
        }
    }

    Err(YubicoError::DeviceNotFound)
}

pub fn wait<F: Fn(Flags) -> bool>(handle: &mut DeviceHandle, f: F, buf: &mut [u8]) -> Result<(), YubicoError>  {
    loop {
        read(handle, buf)?;
        let flags = Flags::from_bits_truncate(buf[7]);
        if flags.contains(Flags::SLOT_WRITE_FLAG) || flags.is_empty() {
            // Should store the version
        }

        if f(flags) {
            return Ok(());
        }
        thread::sleep(Duration::new(0, 1000000));
    }
}

pub fn read(handle: &mut DeviceHandle, buf: &mut [u8]) -> Result<usize, YubicoError> {
    assert_eq!(buf.len(), 8);
    let reqtype = request_type(Direction::In, RequestType::Class, Recipient::Interface);
    let value = REPORT_TYPE_FEATURE << 8;
    Ok(try!(handle.read_control(reqtype, HID_GET_REPORT, value, 0, buf, Duration::new(2, 0))))
}

pub fn write_frame(handle: &mut DeviceHandle, frame: &Frame) -> Result<(), YubicoError> {
    let mut data = unsafe {
        slice::from_raw_parts(frame as *const Frame as *const u8, 70)
    };

    let mut seq = 0;
    let mut buf = [0; 8];
    while !data.is_empty() {
        let (a, b) = data.split_at(7);

        if seq == 0 || b.is_empty() || a.iter().any(|&x| x != 0) {
            let mut packet = [0; 8];
            (&mut packet[ .. 7 ]).copy_from_slice(a);

            packet[7] = Flags::SLOT_WRITE_FLAG.bits() + seq;
            wait(handle, |x| !x.contains(Flags::SLOT_WRITE_FLAG), &mut buf)?;
            raw_write(handle, &packet)?
        }
        data = b;
        seq += 1
    }
    Ok(())
}

pub fn raw_write(handle: &mut DeviceHandle, packet: &[u8]) -> Result<(), YubicoError> {
    let reqtype = request_type(Direction::Out, RequestType::Class, Recipient::Interface);
    let value = REPORT_TYPE_FEATURE << 8;
    if try!(handle.write_control(reqtype, HID_SET_REPORT, value, 0, &packet, Duration::new(2, 0))) != 8 {
        Err(YubicoError::CanNotWriteToDevice)
    } else {
        Ok(())
    }
}

/// Reset the write state after a read.
pub fn write_reset(handle: &mut DeviceHandle) -> Result<(), YubicoError> {
    raw_write(handle, &[0, 0, 0, 0, 0, 0, 0, 0x8f])?;
    let mut buf = [0; 8];
    wait(handle, |x| !x.contains(Flags::SLOT_WRITE_FLAG), &mut buf)?;
    Ok(())
}

pub fn read_response(handle: &mut DeviceHandle, response:&mut [u8]) -> Result<usize, YubicoError> {
    let mut r0 = 0;
    wait(handle, |f| {f.contains(Flags::RESP_PENDING_FLAG)}, &mut response[.. 8])?;
    r0 += 7;
    loop {
        if try!(read(handle, &mut response[r0..r0 + 8])) < 8 {
            break;
        }
        let flags = Flags::from_bits_truncate(response[r0 + 7]);
        if flags.contains(Flags::RESP_PENDING_FLAG) {
            let seq = response[r0 + 7] & 0b00011111;
            if r0 > 0 && seq == 0 {
                // If the sequence number is 0, and we have read at
                // least one packet, stop.
                break;
            }
        } else {
            break;
        }
        r0 += 7;
    }
    try!(write_reset(handle));
    Ok(r0)
}

#[repr(C)]
#[repr(packed)]
pub struct Frame {
    pub payload: [u8; DATA_SIZE],
    command: Command,
    crc: u16,
    filler: [u8; 3],
}

impl Frame {
    pub fn new(payload: [u8; DATA_SIZE], command: Command) -> Self {
        let mut f = Frame {
            payload: payload,
            command: command,
            crc: 0,
            filler: [0; 3],
        };
        f.crc = crc16(&f.payload).to_le();
        f
    }    
}