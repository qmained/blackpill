use crate::{PAYLOAD_SIGNAL, REQUEST_NEXT_PAYLOAD_SIGNAL};
use defmt::{error, info};
use embassy_stm32::peripherals::USB_OTG_FS;
use embassy_stm32::usb::Driver;
use embassy_usb::class::cdc_acm::CdcAcmClass;

pub const PAYLOAD_SIZE: usize = 128;

#[embassy_executor::task]
pub async fn get_next_payload(
    class: &'static mut CdcAcmClass<'static, Driver<'static, USB_OTG_FS>>,
    video_length: u64,
) -> ! {
    loop {
        let mut index = REQUEST_NEXT_PAYLOAD_SIGNAL.wait().await;
        let mut input_buffer = [0u8; PAYLOAD_SIZE];
        let size = video_length.saturating_sub(index).min(PAYLOAD_SIZE as u64) as usize;
        let cycles = size.div_ceil(64);
        let last_cycle = if size % 64 != 0 { size % 64 } else { 64 };

        let mut i = 0;
        let mut write_buf = [0u8; 10];
        info!(
            "index: {}, size: {}, cycles: {}, last_cycle: {}",
            index, size, cycles, last_cycle
        );
        'outer: while i < cycles {
            info!("Loop in payload request, i: {}", i);
            let current_packet_size = if i == cycles - 1 { last_cycle } else { 64 } as u16;
            write_buf[0..8].copy_from_slice(&index.to_be_bytes());
            write_buf[8..10].copy_from_slice(&current_packet_size.to_be_bytes());
            if class.write_packet(&write_buf).await.is_ok() {
                let offset = i * 64;
                loop {
                    match class
                        .read_packet(
                            &mut input_buffer[offset..offset + current_packet_size as usize],
                        )
                        .await
                    {
                        Ok(0) => {
                            info!("Got zero bytes");
                            continue;
                        }
                        Ok(bytes) => {
                            info!("Got {} bytes", bytes);
                            i += 1;
                            index += bytes as u64;
                            if bytes < 64 {
                                break 'outer;
                            }
                            break;
                        }
                        Err(e) => {
                            error!("Error!!: {}", e);
                            break 'outer;
                        }
                    }
                }
            }
        }
        PAYLOAD_SIGNAL.signal((input_buffer, size));
    }
}
